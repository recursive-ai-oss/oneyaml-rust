//! The `serde-*` and `ser-*` sections of `docs/performance.md`: the
//! serde path, reading (`serde-*`, [`crate::serde_run`]) and writing
//! (`ser-*`, [`crate::ser_run`]), against the YAML serde comparators, with
//! serde_json as a reference.
//!
//! Every section first runs the direction's correctness guard for every
//! comparator in a child process (`bench serde-check` / `bench ser-check`);
//! a cell whose library disagrees with ours, rejects the input, or can't do
//! the equivalent work shows why instead of a number. Measurements reuse
//! the parser tables' child-process probes: a probe name
//! `serde:<lib>:<workload>` selects
//! [`crate::serde_run::measured_drain_serde`], `ser:<lib>:<workload>`
//! selects [`crate::ser_run::measured_drain_ser`].
//!
//! **Gated ratio.** The serde target (≤ 1.25×) is measured against
//! the fastest *feature-complete* comparator on the row
//! ([`Lib::feature_complete`]: serde_yaml_ng, serde_norway, serde-saphyr).
//! serde_yaml2 is partial (no multi-document API, no merge keys): its
//! column and its own ratio stay visible, but it never sets the verdict.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use oracles::serde_cmp::Lib;

use crate::corpus::Case;
use crate::markdown::{header, parallel, thousands};
use crate::report::{Env, fmt_ratio, per_byte, show};
use crate::ser_run::{self, SerProbe, SerWorkload};
use crate::serde_run::{self, SerdeLib, Verdict, Workload, cases, measured_drain_serde};
use crate::valgrind::{self, Heap, Outcome};

/// The serde sections, in document order.
pub const SECTIONS: &[&str] = &[
    "serde-check",
    "serde-instructions",
    "serde-allocations",
    "serde-wall",
    "ser-check",
    "ser-instructions",
    "ser-allocations",
    "ser-wall",
];

/// The serde target: ours ≤ this × the fastest feature-complete
/// comparator.
pub const TARGET: f64 = 1.25;

/// Which direction of the serde path a section measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Dir {
    /// Deserializing (`serde-*`).
    Read,
    /// Serializing (`ser-*`).
    Write,
}

impl Dir {
    /// The guard subcommand.
    const fn check_command(self) -> &'static str {
        match self {
            Dir::Read => "serde-check",
            Dir::Write => "ser-check",
        }
    }

    fn workloads(self) -> Vec<&'static str> {
        match self {
            Dir::Read => Workload::MEASURED.iter().map(|w| w.name()).collect(),
            Dir::Write => SerWorkload::MEASURED.iter().map(|w| w.name()).collect(),
        }
    }

    /// The probe measuring `lib` on `workload`.
    fn probe(self, lib: SerdeLib, workload: &str) -> String {
        match self {
            Dir::Read => format!("serde:{}:{workload}", lib.name()),
            Dir::Write => format!("ser:{}:{workload}", lib.name()),
        }
    }

    /// The probe whose DHAT figures are subtracted from [`Dir::probe`]'s:
    /// the same input preparation, without the measured work.
    fn baseline(self, lib: SerdeLib, workload: &str) -> String {
        match self {
            // Generating the YAML, or reading serde_json's JSON file.
            Dir::Read if lib.is_json() => probe_name(lib, Workload::Baseline),
            Dir::Read => probe_name(SerdeLib::OneYaml, Workload::Baseline),
            // The data prepared for `lib`, written by no one.
            Dir::Write => format!("ser:{}:{workload}:baseline", lib.name()),
        }
    }
}

/// Builds one serde section, or `None` if `name` isn't one.
#[must_use]
pub fn section(name: &str, env: &Env) -> Option<Result<String, String>> {
    let (dir, what) = if let Some(w) = name.strip_prefix("serde-") {
        (Dir::Read, w)
    } else {
        (Dir::Write, name.strip_prefix("ser-")?)
    };
    Some(match what {
        "check" => check_table(dir, env),
        "instructions" => instructions_table(dir, env),
        "allocations" => allocations_table(dir, env),
        "wall" => wall_table(dir, env),
        _ => return None,
    })
}

/// The probe name for `lib` reading `workload`.
#[must_use]
pub fn probe_name(lib: SerdeLib, workload: Workload) -> String {
    format!("serde:{}:{}", lib.name(), workload.name())
}

/// Parses [`probe_name`].
#[must_use]
pub fn parse_probe_name(name: &str) -> Option<(SerdeLib, Workload)> {
    let rest = name.strip_prefix("serde:")?;
    let (lib, workload) = rest.split_once(':')?;
    Some((SerdeLib::from_name(lib)?, Workload::from_name(workload)?))
}

type Row = (&'static Case, &'static str);

fn rows(dir: Dir) -> Vec<Row> {
    let ws = dir.workloads();
    cases()
        .into_iter()
        .flat_map(|c| ws.iter().map(move |w| (c, *w)))
        .collect()
}

/// A guard verdict and, for the write direction, the output length.
type Checked = (Verdict, Option<usize>);

/// Guard verdicts, `[row][lib]` over [`SerdeLib::ALL`]. Ours is checked
/// too in the write direction (its output must read back); in the read
/// direction it is the reference and always agrees.
fn verdicts(dir: Dir, env: &Env, rows: &[Row]) -> Result<Vec<Vec<Checked>>, String> {
    let skip = usize::from(dir == Dir::Read);
    let work: Vec<(Row, SerdeLib)> = rows
        .iter()
        .flat_map(|r| SerdeLib::ALL.iter().skip(skip).map(move |l| (*r, *l)))
        .collect();
    let flat = parallel(&work, env.jobs, |((c, w), l)| {
        let args = [*w, c.name];
        let out = valgrind::run_child(
            &env.exe,
            &[dir.check_command()],
            Path::new(l.name()),
            &args,
            env.timeout,
        )?;
        Ok::<_, String>(match out {
            Outcome::Ok((stdout, _)) => {
                let mut lines = stdout.lines();
                let verdict = lines
                    .next()
                    .and_then(Verdict::parse)
                    .ok_or_else(|| format!("bad {} output: {stdout:?}", dir.check_command()))?;
                (verdict, lines.next().and_then(|l| l.trim().parse().ok()))
            }
            other => (Verdict::Rejects(show(&other, |_| String::new())), None),
        })
    });
    let mut it = flat.into_iter();
    let mut out = Vec::with_capacity(rows.len());
    for _ in rows {
        let mut row = Vec::with_capacity(SerdeLib::ALL.len());
        if skip == 1 {
            row.push((Verdict::Agrees, None));
        }
        for _ in skip..SerdeLib::ALL.len() {
            row.push(it.next().ok_or("missing verdict")??);
        }
        out.push(row);
    }
    Ok(out)
}

/// Why a cell has no number, or `None` if it is measured.
fn excluded(row: &[Checked], lib: usize) -> Option<String> {
    if row
        .iter()
        .any(|(v, _)| matches!(v, Verdict::OursRejects(_)))
    {
        return Some("ours rejects".to_owned());
    }
    match &row.get(lib)?.0 {
        Verdict::Agrees => None,
        Verdict::Differs(_) => Some("differs".to_owned()),
        Verdict::Unsupported(_) => Some("n/a".to_owned()),
        Verdict::Rejects(_) | Verdict::OursRejects(_) => Some("rejects".to_owned()),
    }
}

/// A measured grid: `[row][lib]`, `Err(reason)` for an excluded cell.
type Grid<T> = Vec<Vec<Result<Outcome<T>, String>>>;

fn grid<T: Send>(
    rows: &[Row],
    verdicts: &[Vec<Checked>],
    jobs: usize,
    measure: impl Fn(SerdeLib, &str, &Case) -> Result<Outcome<T>, String> + Sync,
) -> Result<Grid<T>, String> {
    let work: Vec<(usize, usize)> = (0..rows.len())
        .flat_map(|r| (0..SerdeLib::ALL.len()).map(move |l| (r, l)))
        .collect();
    let flat = parallel(&work, jobs, |(r, l)| {
        let (Some((c, w)), Some(lib), Some(v)) =
            (rows.get(*r), SerdeLib::ALL.get(*l), verdicts.get(*r))
        else {
            return Ok(Err("–".to_owned()));
        };
        match excluded(v, *l) {
            Some(why) => Ok(Err(why)),
            None => measure(*lib, w, c).map(Ok),
        }
    });
    let mut it = flat.into_iter();
    let mut out = Vec::with_capacity(rows.len());
    for _ in rows {
        let row: Result<Vec<_>, String> = it.by_ref().take(SerdeLib::ALL.len()).collect();
        out.push(row?);
    }
    Ok(out)
}

fn value<T: Clone>(cell: Option<&Result<Outcome<T>, String>>) -> Option<T> {
    cell.and_then(|c| c.as_ref().ok())
        .and_then(|o| o.clone().ok())
}

fn cell<T>(c: &Result<Outcome<T>, String>, f: impl Fn(&T) -> String) -> String {
    match c {
        Ok(o) => show(o, f),
        Err(why) => why.clone(),
    }
}

/// The ratio columns of one lower-is-better row of values over
/// [`SerdeLib::ALL`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ratios {
    /// Ours ÷ the best feature-complete comparator (the gated ratio).
    gated: String,
    /// That comparator's name.
    best: String,
    /// `met` / `**missed**` against [`TARGET`], or `–`.
    verdict: String,
    /// Ours ÷ serde_yaml2 (partial: reported, not gated).
    partial: String,
    /// Ours ÷ serde_json.
    json: String,
}

fn ratios(vals: &[Option<f64>]) -> Ratios {
    let ours = vals.first().copied().flatten();
    let at = |lib: SerdeLib| {
        SerdeLib::ALL
            .iter()
            .position(|l| *l == lib)
            .and_then(|i| vals.get(i).copied().flatten())
    };
    let best = SerdeLib::ALL
        .iter()
        .zip(vals)
        .filter(|(l, _)| matches!(l, SerdeLib::Other(o) if o.feature_complete()))
        .filter_map(|(l, v)| v.map(|v| (*l, v)))
        .reduce(|a, b| if b.1 < a.1 { b } else { a });
    let ratio = |b: Option<f64>| match (ours, b) {
        (Some(o), Some(b)) if b > 0.0 => Some(o / b),
        _ => None,
    };
    let text = |r: Option<f64>| r.map_or_else(|| "–".to_owned(), fmt_ratio);
    let gated = ratio(best.map(|b| b.1));
    // Compare what is printed, so a verdict never contradicts its cell.
    let verdict = gated.map_or_else(
        || "–".to_owned(),
        |g| {
            let shown: f64 = fmt_ratio(g).trim_end_matches('×').parse().unwrap_or(g);
            if shown <= TARGET {
                "met".to_owned()
            } else {
                "**missed**".to_owned()
            }
        },
    );
    Ratios {
        gated: text(gated),
        best: best.map_or("–", |(l, _)| l.name()).to_owned(),
        verdict,
        partial: text(ratio(at(SerdeLib::Other(Lib::Yaml2)))),
        json: text(ratio(at(SerdeLib::Other(Lib::Json)))),
    }
}

fn lib_columns(prefix: &[&str], suffix: &str, tail: &[&str]) -> String {
    let mut cols: Vec<String> = prefix.iter().map(|s| (*s).to_owned()).collect();
    cols.extend(
        SerdeLib::ALL
            .iter()
            .map(|l| format!("{}{suffix}", l.name())),
    );
    cols.extend(tail.iter().map(|s| (*s).to_owned()));
    header(&cols)
}

fn check_table(dir: Dir, env: &Env) -> Result<String, String> {
    let rows = rows(dir);
    let v = verdicts(dir, env, &rows)?;
    let mut s = lib_columns(&["Case", "Workload"], "", &[]);
    let mut notes = Vec::new();
    for ((c, w), row) in rows.iter().zip(&v) {
        let bytes = c.generate(1).len();
        let _ = write!(s, "| {} | {w} |", c.name);
        for (i, (lib, (verdict, len))) in SerdeLib::ALL.iter().zip(row).enumerate() {
            let text = match verdict {
                Verdict::Agrees => match len {
                    Some(n) => format!("agrees ({:.2}×)", per_byte(*n as u64, bytes)),
                    None => "agrees".to_owned(),
                },
                Verdict::Differs(d) | Verdict::Unsupported(d) | Verdict::Rejects(d) => {
                    notes.push(format!("{}, {} {w}: {d}", lib.name(), c.name));
                    let why = excluded(row, i).unwrap_or_default();
                    format!("{why} [{}]", notes.len())
                }
                Verdict::OursRejects(d) => format!("**ours rejects**: {d}"),
            };
            let _ = write!(s, " {text} |");
        }
        s.push('\n');
    }
    s.push('\n');
    for (i, n) in notes.iter().enumerate() {
        let _ = writeln!(s, "{}. {}", i.saturating_add(1), n.replace('`', "'"));
    }
    Ok(s)
}

const IR_TAIL: [&str; 5] = [
    "oneyaml ÷ fastest feature-complete (target applies)",
    "fastest feature-complete",
    "target ≤ 1.25×",
    "oneyaml ÷ serde_yaml2 (partial, no target)",
    "oneyaml ÷ serde_json",
];

fn instructions_table(dir: Dir, env: &Env) -> Result<String, String> {
    let rows = rows(dir);
    let v = verdicts(dir, env, &rows)?;
    let g = grid(&rows, &v, env.jobs, |l, w, c| {
        valgrind::instructions(
            &env.valgrind,
            &env.exe,
            &dir.probe(l, w),
            c.name,
            1,
            env.timeout,
        )
    })?;
    let mut s = lib_columns(&["Case", "Workload", "Bytes"], " Ir/B", &IR_TAIL);
    for ((c, w), row) in rows.iter().zip(&g) {
        let bytes = c.generate(1).len();
        let rate = |ir: &u64| per_byte(*ir, bytes);
        let _ = write!(s, "| {} | {w} | {} |", c.name, thousands(bytes as u64));
        for x in row {
            let _ = write!(s, " {} |", cell(x, |ir| format!("{:.1}", rate(ir))));
        }
        let vals: Vec<Option<f64>> = row
            .iter()
            .map(|x| value(Some(x)).map(|ir| rate(&ir)))
            .collect();
        let r = ratios(&vals);
        let _ = writeln!(
            s,
            " {} | {} | {} | {} | {} |",
            r.gated, r.best, r.verdict, r.partial, r.json
        );
    }
    Ok(s)
}

/// One DHAT sub-table: title, value from `(run, baseline, input bytes)`
/// (`None`: nothing to show, see [`HIDDEN`]), cell format, whether to
/// show the baseline's peak as a column, and whether to show the ratio
/// columns.
type HeapTable = (
    &'static str,
    fn(Heap, Heap, usize) -> Option<f64>,
    fn(f64) -> String,
    bool,
    bool,
);

/// The write-peak cell when the whole-process peak is the baseline's (the
/// preparation's transient): writing stayed below it, so DHAT cannot tell
/// what it added.
const HIDDEN: &str = "≤ prep";

const COUNT: HeapTable = (
    "Allocation count (blocks, `realloc` included; baseline subtracted)",
    |h, b, _| Some(h.minus(b).total_blocks as f64),
    |x| thousands(x as u64),
    false,
    true,
);

const BYTES: HeapTable = (
    "Bytes allocated ÷ YAML input bytes (baseline subtracted)",
    |h, b, n| Some(per_byte(h.minus(b).total_bytes, n)),
    |x| format!("{x:.2}"),
    false,
    true,
);

const READ_HEAP_TABLES: [HeapTable; 3] = [
    COUNT,
    BYTES,
    (
        // Whole-process, like the parser tables: a baseline's peak can't be
        // subtracted (it happens during input generation, not beside the
        // deserialized value). serde_json's input is the JSON file, read.
        "Whole-process peak heap ÷ YAML input bytes (the baseline column is YAML generation alone)",
        |h, _, n| Some(per_byte(h.peak_bytes, n)),
        |x| format!("{x:.2}"),
        true,
        true,
    ),
];

const WRITE_HEAP_TABLES: [HeapTable; 3] = [
    COUNT,
    BYTES,
    (
        // The baseline's peak is the prepared data (or, when larger, the
        // transient of reading it); writing happens with the data live, so
        // the difference is what writing adds at its peak: the output plus
        // the writer's working set (a lower bound when the transient
        // dominates; hidden, and shown as `HIDDEN`, when writing never
        // reaches it). No ratio columns: a hidden cell is an unknown value
        // below its preparation's peak, so a "best" among the visible
        // cells could be beaten by a hidden one.
        "Peak heap added by writing ÷ YAML input bytes (whole-process peak minus the baseline's, which holds the prepared data)",
        |h, b, n| {
            (h.peak_bytes > b.peak_bytes)
                .then(|| per_byte(h.peak_bytes.saturating_sub(b.peak_bytes), n))
        },
        |x| format!("{x:.2}"),
        false,
        false,
    ),
];

const HEAP_TAIL: [&str; 3] = [
    "oneyaml ÷ best feature-complete",
    "best feature-complete",
    "oneyaml ÷ serde_yaml2 (partial)",
];

fn allocations_table(dir: Dir, env: &Env) -> Result<String, String> {
    let rows = rows(dir);
    let v = verdicts(dir, env, &rows)?;
    let heap = |probe: &str, c: &Case| {
        valgrind::heap(&env.valgrind, &env.exe, probe, c.name, 1, env.timeout)
    };
    let g = grid(&rows, &v, env.jobs, |l, w, c| heap(&dir.probe(l, w), c))?;
    // The distinct baseline probes of every measured cell.
    let mut wanted: Vec<(String, &'static Case)> = Vec::new();
    for (r, (c, w)) in rows.iter().enumerate() {
        for (i, lib) in SerdeLib::ALL.iter().enumerate() {
            let measured = v.get(r).is_some_and(|row| excluded(row, i).is_none());
            let key = (dir.baseline(*lib, w), *c);
            if measured && !wanted.iter().any(|k| k.0 == key.0 && k.1.name == c.name) {
                wanted.push(key);
            }
        }
    }
    let bases: Vec<Heap> = parallel(&wanted, env.jobs, |(p, c)| match heap(p, c)? {
        Outcome::Ok(h) => Ok(h),
        other => Err(format!(
            "baseline probe {p} failed for {}: {}",
            c.name,
            show(&other, |_| String::new())
        )),
    })
    .into_iter()
    .collect::<Result<_, String>>()?;
    let base_of = |lib: SerdeLib, w: &str, c: &Case| {
        let p = dir.baseline(lib, w);
        wanted
            .iter()
            .position(|k| k.0 == p && k.1.name == c.name)
            .and_then(|i| bases.get(i).copied())
    };
    let tables = match dir {
        Dir::Read => READ_HEAP_TABLES,
        Dir::Write => WRITE_HEAP_TABLES,
    };
    let yaml_base = |c: &Case| base_of(SerdeLib::OneYaml, "", c);
    let mut s = String::new();
    for (title, f, fmt, with_baseline, with_ratios) in tables {
        let _ = writeln!(s, "**{title}**\n");
        let prefix: &[&str] = if with_baseline {
            &["Case", "Workload", "Bytes", "baseline"]
        } else {
            &["Case", "Workload", "Bytes"]
        };
        let tail: &[&str] = if with_ratios { &HEAP_TAIL } else { &[] };
        s.push_str(&lib_columns(prefix, "", tail));
        for ((c, w), row) in rows.iter().zip(&g) {
            let bytes = c.generate(1).len();
            let _ = write!(s, "| {} | {w} | {} |", c.name, thousands(bytes as u64));
            if with_baseline {
                let b = yaml_base(c).ok_or_else(|| format!("no baseline for {}", c.name))?;
                let _ = write!(s, " {} |", fmt(per_byte(b.peak_bytes, bytes)));
            }
            let mut vals = Vec::new();
            for (lib, x) in SerdeLib::ALL.iter().zip(row) {
                let base = base_of(*lib, w, c);
                let text = match (x, base) {
                    (Ok(_), None) => return Err(format!("no baseline for {}", c.name)),
                    (_, b) => cell(x, |h| {
                        f(*h, b.unwrap_or_default(), bytes).map_or_else(|| HIDDEN.to_owned(), fmt)
                    }),
                };
                let _ = write!(s, " {text} |");
                vals.push(value(Some(x)).zip(base).and_then(|(h, b)| f(h, b, bytes)));
            }
            if with_ratios {
                let r = ratios(&vals);
                let _ = write!(s, " {} | {} | {} |", r.gated, r.best, r.partial);
            }
            s.push('\n');
        }
        s.push('\n');
    }
    Ok(s)
}

/// Median wall-clock time of one [`measured_drain_serde`] call and the
/// sample count: at least 5, then more until `budget` has elapsed (≤ 1000).
#[must_use]
pub fn wall_clock(
    lib: SerdeLib,
    workload: Workload,
    case: &Case,
    input: &str,
    budget: Duration,
) -> (Duration, usize) {
    median(budget, || {
        let _ = measured_drain_serde(lib, workload, case, input);
    })
}

/// Median wall-clock time of one call of `f` and the sample count: one
/// warm-up, at least 5 samples, then more until `budget` has elapsed
/// (≤ 1000).
pub fn median(budget: Duration, mut f: impl FnMut()) -> (Duration, usize) {
    f(); // warm-up
    let start = Instant::now();
    let mut samples = Vec::new();
    while samples.len() < 5 || (start.elapsed() < budget && samples.len() < 1000) {
        let t = Instant::now();
        f();
        samples.push(t.elapsed());
    }
    samples.sort_unstable();
    let median = samples.get(samples.len() / 2).copied().unwrap_or_default();
    (median, samples.len())
}

const WALL_TAIL: [&str; 4] = [
    "oneyaml ÷ fastest feature-complete",
    "fastest feature-complete",
    "oneyaml ÷ serde_yaml2 (partial)",
    "oneyaml ÷ serde_json",
];

fn wall_table(dir: Dir, env: &Env) -> Result<String, String> {
    let rows = rows(dir);
    let v = verdicts(dir, env, &rows)?;
    let limit = env.timeout.checked_div(4).unwrap_or_default();
    // One job: concurrent timing runs would disturb each other.
    let g = grid(&rows, &v, 1, |l, w, c| {
        let args = [c.name, "1", "300"];
        let name = dir.probe(l, w);
        match valgrind::run_child(&env.exe, &["time"], Path::new(&name), &args, limit)? {
            Outcome::Ok((stdout, _)) => stdout
                .split_whitespace()
                .next()
                .and_then(|w| w.parse::<f64>().ok())
                .map(Outcome::Ok)
                .ok_or_else(|| format!("bad `time` output: {stdout:?}")),
            other => Ok(other.map(|_| 0.0)),
        }
    })?;
    let mut s = crate::wall::host_line();
    s.push_str(&lib_columns(
        &["Case", "Workload", "Bytes"],
        " MB/s",
        &WALL_TAIL,
    ));
    for ((c, w), row) in rows.iter().zip(&g) {
        let bytes = c.generate(1).len();
        let mbps = |ns: &f64| format!("{:.0}", bytes as f64 * 1e3 / ns.max(1.0));
        let _ = write!(s, "| {} | {w} | {} |", c.name, thousands(bytes as u64));
        for x in row {
            let _ = write!(s, " {} |", cell(x, mbps));
        }
        let vals: Vec<Option<f64>> = row.iter().map(|x| value(Some(x))).collect();
        let r = ratios(&vals);
        let _ = writeln!(
            s,
            " {} | {} | {} | {} |",
            r.gated, r.best, r.partial, r.json
        );
    }
    Ok(s)
}

/// `bench serde-check <lib> <workload> <case>`: the read guard's verdict
/// line.
///
/// # Errors
/// Unknown library, workload or case.
pub fn check_command(args: &[String]) -> Result<String, String> {
    let [lib, workload, case] = args else {
        return Err("serde-check <lib> <workload> <case>".to_owned());
    };
    let lib = SerdeLib::from_name(lib).ok_or_else(|| format!("unknown library `{lib}`"))?;
    let w =
        Workload::from_name(workload).ok_or_else(|| format!("unknown workload `{workload}`"))?;
    let case = crate::corpus::find(case).ok_or_else(|| format!("unknown case `{case}`"))?;
    Ok(serde_run::check(lib, w, case).line())
}

/// `bench ser-check <lib> <workload> <case>`: the write guard's verdict
/// line, then the output length on a second line when there is output.
///
/// # Errors
/// Unknown library, workload or case.
pub fn ser_check_command(args: &[String]) -> Result<String, String> {
    let [lib, workload, case] = args else {
        return Err("ser-check <lib> <workload> <case>".to_owned());
    };
    let lib = SerdeLib::from_name(lib).ok_or_else(|| format!("unknown library `{lib}`"))?;
    let w =
        SerWorkload::from_name(workload).ok_or_else(|| format!("unknown workload `{workload}`"))?;
    let case = crate::corpus::find(case).ok_or_else(|| format!("unknown case `{case}`"))?;
    let (verdict, len) = ser_run::check(lib, w, case);
    Ok(match len {
        Some(n) => format!("{}\n{n}", verdict.line()),
        None => verdict.line(),
    })
}

/// A `ser:` probe's parts: the probe, the case, and the prepared data.
pub type SerInput = (SerProbe, &'static Case, ser_run::Data);

/// Parses a `ser:` probe's `<probe> <case> <scale>` and prepares its data
/// (not measured). `Ok(None)` when the name is not a `ser:` probe.
///
/// # Errors
/// Unknown case, a scale other than 1, or preparing failed.
pub fn ser_input(args: &[String]) -> Result<Option<SerInput>, String> {
    let [name, case, scale, ..] = args else {
        return Err("<probe> <case> <scale>".to_owned());
    };
    let Some(probe) = SerProbe::parse(name) else {
        return Ok(None);
    };
    let case = crate::corpus::find(case).ok_or_else(|| format!("unknown case `{case}`"))?;
    if scale != "1" {
        return Err("serde probes run at scale 1".to_owned());
    }
    let data = ser_run::prepare(probe.lib, probe.workload, case)
        .map_err(|e| format!("{}: preparing: {e:?}", probe.name()))?;
    Ok(Some((probe, case, data)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn probe_names_round_trip() {
        for l in SerdeLib::ALL {
            for w in Workload::MEASURED {
                assert_eq!(parse_probe_name(&probe_name(*l, *w)), Some((*l, *w)));
                assert_eq!(Dir::Read.probe(*l, w.name()), probe_name(*l, *w));
            }
            for w in SerWorkload::MEASURED {
                let p = SerProbe::parse(&Dir::Write.probe(*l, w.name())).unwrap();
                assert_eq!((p.lib, p.workload, p.baseline), (*l, *w, false));
                let b = SerProbe::parse(&Dir::Write.baseline(*l, w.name())).unwrap();
                assert!(b.baseline);
            }
        }
        assert_eq!(parse_probe_name("oneyaml"), None);
        assert_eq!(parse_probe_name("serde:nope:value"), None);
        let json = SerdeLib::Other(Lib::Json);
        assert_eq!(
            Dir::Read.baseline(json, "value"),
            "serde:serde_json:baseline"
        );
        let ng = SerdeLib::Other(Lib::YamlNg);
        assert_eq!(Dir::Read.baseline(ng, "typed"), "serde:oneyaml:baseline");
    }

    #[test]
    fn section_names() {
        let env = Env {
            valgrind: "valgrind".into(),
            exe: "x".into(),
            jobs: 1,
            timeout: Duration::from_secs(1),
            provenance: crate::readme::Provenance::default(),
        };
        assert!(section("serde-bogus", &env).is_none());
        assert!(section("ser-bogus", &env).is_none());
        assert!(section("corpus", &env).is_none());
        assert_eq!(rows(Dir::Read).len(), cases().len() * 3);
        assert_eq!(rows(Dir::Write).len(), cases().len() * 2);
    }

    /// The gated ratio ignores serde_yaml2 and serde_json, which stay
    /// visible in their own columns; the verdict follows the printed ratio.
    #[test]
    fn gated_ratio_is_feature_complete_only() {
        // ALL order: oneyaml, ng, norway, yaml2, saphyr, json.
        let vals = [
            Some(6.0),
            Some(5.0),
            Some(4.0),
            Some(1.0),
            Some(8.0),
            Some(0.5),
        ];
        let r = ratios(&vals);
        assert_eq!(
            r,
            Ratios {
                gated: "1.50×".into(),
                best: "serde_norway".into(),
                verdict: "**missed**".into(),
                partial: "6.00×".into(),
                json: "12.00×".into(),
            }
        );
        let vals = [Some(5.0), Some(4.0), None, Some(1.0), None, None];
        let r = ratios(&vals);
        assert_eq!((r.gated.as_str(), r.verdict.as_str()), ("1.25×", "met"));
        assert_eq!(r.best, "serde_yaml_ng");
        // 1.2549 prints as 1.25× and is met; 1.2551 prints 1.26×.
        assert_eq!(ratios(&[Some(1.2549), Some(1.0)]).verdict, "met");
        assert_eq!(ratios(&[Some(1.2551), Some(1.0)]).verdict, "**missed**");
        let none = ratios(&[None, Some(1.0)]);
        assert_eq!((none.gated.as_str(), none.verdict.as_str()), ("–", "–"));
        let only_partial = ratios(&[Some(1.0), None, None, Some(1.0)]);
        assert_eq!(only_partial.best, "–");
        assert_eq!(only_partial.partial, "1.00×");
    }

    #[test]
    fn write_peak_is_hidden_below_the_preparation() {
        let (_, peak, _, _, ratios) = WRITE_HEAP_TABLES[2];
        assert!(!ratios);
        assert!(READ_HEAP_TABLES.iter().all(|t| t.4));
        let h = |p| Heap {
            peak_bytes: p,
            ..Heap::default()
        };
        assert_eq!(peak(h(300), h(100), 100), Some(2.0));
        assert_eq!(peak(h(100), h(100), 100), None);
        assert_eq!(peak(h(50), h(100), 100), None);
        let (_, count, _, _, _) = WRITE_HEAP_TABLES[0];
        let blocks = Heap {
            total_blocks: 7,
            ..Heap::default()
        };
        assert_eq!(count(blocks, Heap::default(), 1), Some(7.0));
    }

    #[test]
    fn exclusions() {
        let row = [
            (Verdict::Agrees, None),
            (Verdict::Differs("x".into()), None),
            (Verdict::Unsupported("y".into()), None),
        ];
        assert_eq!(excluded(&row, 0), None);
        assert_eq!(excluded(&row, 1).as_deref(), Some("differs"));
        assert_eq!(excluded(&row, 2).as_deref(), Some("n/a"));
        let bad = [
            (Verdict::Agrees, None),
            (Verdict::OursRejects("z".into()), None),
        ];
        assert_eq!(excluded(&bad, 0).as_deref(), Some("ours rejects"));
    }

    #[test]
    fn check_commands_validate_arguments() {
        assert!(check_command(&[]).is_err());
        assert!(ser_check_command(&[]).is_err());
        let args = |a: &str, b: &str, c: &str| [a.to_owned(), b.to_owned(), c.to_owned()];
        assert!(check_command(&args("x", "value", "gha-workflow")).is_err());
        assert!(check_command(&args("oneyaml", "x", "gha-workflow")).is_err());
        assert!(check_command(&args("oneyaml", "value", "x")).is_err());
        assert!(ser_check_command(&args("x", "value", "gha-workflow")).is_err());
        assert!(ser_check_command(&args("oneyaml", "sparse", "gha-workflow")).is_err());
        assert!(ser_check_command(&args("oneyaml", "value", "x")).is_err());
        assert_eq!(
            check_command(&args("serde_json", "sparse", "gha-workflow")).unwrap(),
            "agrees"
        );
        let out = ser_check_command(&args("serde_norway", "typed", "gha-workflow")).unwrap();
        let mut lines = out.lines();
        assert_eq!(lines.next(), Some("agrees"));
        assert!(lines.next().unwrap().parse::<usize>().unwrap() > 1000);
        let out = ser_check_command(&args("serde_yaml2", "value", "k8s-manifest")).unwrap();
        assert!(out.starts_with("unsupported"), "{out}");
    }

    #[test]
    fn ser_inputs() {
        let a = |x: &str, y: &str, z: &str| vec![x.to_owned(), y.to_owned(), z.to_owned()];
        assert!(
            ser_input(&a("oneyaml", "gha-workflow", "1"))
                .unwrap()
                .is_none()
        );
        assert!(ser_input(&a("ser:oneyaml:value", "nope", "1")).is_err());
        assert!(ser_input(&a("ser:oneyaml:value", "gha-workflow", "2")).is_err());
        assert!(ser_input(&a("ser:serde_yaml2:value", "k8s-manifest", "1")).is_err());
        let (p, c, data) = ser_input(&a("ser:oneyaml:typed:baseline", "gha-workflow", "1"))
            .unwrap()
            .unwrap();
        assert!(p.baseline);
        assert_eq!((c.name, data.len()), ("gha-workflow", 1));
        assert!(ser_input(&[]).is_err());
    }
}
