//! The `docs/performance.md` tables.
//!
//! Each table lives between `<!-- bench:NAME:begin -->` and
//! `<!-- bench:NAME:end -->` markers; [`update`] replaces only what is
//! between them, so the prose around the tables is hand-written. Every
//! measurement of every parser runs in a child process, so a comparator
//! that rejects an input or crashes shows up as a cell, not a failed run.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::corpus::{CASES, Case, Kind, fnv1a};
use crate::markdown::{header, parallel, thousands};
use crate::valgrind::{self, Heap, Outcome};
use crate::{Parser, measured_drain};

/// The regenerable sections, in document order.
pub const SECTIONS: &[&str] = &[
    crate::readme::ENVIRONMENT_SECTION,
    "corpus",
    "instructions",
    "allocations",
    "collect",
    "tree",
    "scaling",
    "wall",
    "wall-scaling",
    "serde-check",
    "serde-instructions",
    "serde-allocations",
    "serde-wall",
    "ser-check",
    "ser-instructions",
    "ser-allocations",
    "ser-wall",
    "document",
    "edits",
];

/// Scales used by the linear-scaling check.
pub const SCALES: [usize; 3] = [1, 10, 100];

/// `(ratio at 100×) > SUPERLINEAR` flags a shape as superlinear.
pub const SUPERLINEAR: f64 = 1.25;

/// Where valgrind and the probe binary live, and how many jobs run at once.
#[derive(Clone, Debug)]
pub struct Env {
    /// The `valgrind` executable.
    pub valgrind: PathBuf,
    /// This binary (re-run as `probe` / `time`).
    pub exe: PathBuf,
    /// Parallel valgrind processes.
    pub jobs: usize,
    /// Per-child limit for valgrind probes (wall-clock `time` children get
    /// a quarter of it).
    pub timeout: Duration,
    /// Recorded-provenance overrides for the `environment` section.
    pub provenance: crate::readme::Provenance,
}

/// Scale in the deterministic tables: real-world as is, stress at 10×.
#[must_use]
pub const fn main_scale(case: &Case) -> usize {
    match case.kind {
        Kind::RealWorld => 1,
        Kind::Stress => 10,
    }
}

/// Scale in the wall-clock table: real-world as is, stress at 100× (~1 MB).
#[must_use]
pub const fn wall_scale(case: &Case) -> usize {
    match case.kind {
        Kind::RealWorld => 1,
        Kind::Stress => 100,
    }
}

/// Builds one section's markdown.
///
/// # Errors
/// Unknown section, or a measurement that could not be taken at all.
pub fn section(name: &str, env: &Env) -> Result<String, String> {
    match name {
        crate::readme::ENVIRONMENT_SECTION => crate::readme::environment_section(&env.provenance),
        "corpus" => Ok(corpus_table()),
        "instructions" => instructions_table(env),
        "allocations" => allocations_table(env),
        "collect" => collect_table(env),
        "tree" => tree_table(env),
        "scaling" => scaling_table(env),
        "wall" => crate::wall::wall_table(env),
        "wall-scaling" => crate::wall::wall_scaling_table(env),
        other => crate::serde_report::section(other, env)
            .or_else(|| crate::document::section(other, env))
            .unwrap_or_else(|| {
                Err(format!(
                    "unknown section `{other}` (known: {})",
                    SECTIONS.join(", ")
                ))
            }),
    }
}

/// Regenerates `sections` of the markdown file at `path`.
///
/// # Errors
/// As [`section`], plus I/O and marker errors, or a section whose text
/// names an internal process identifier ([`oracles::public_text`]): every
/// section is published in `docs/`.
pub fn update(path: &Path, sections: &[&str], env: &Env) -> Result<(), String> {
    update_with(path, sections, |name| section(name, env))
}

/// [`update`] with `build` producing each section's body: every body is
/// checked for internal process identifiers before its section is
/// written, so a refused section leaves the file as it was.
///
/// # Errors
/// As `build`, plus I/O and marker errors, or a body that names an
/// internal process identifier.
pub fn update_with(
    path: &Path,
    sections: &[&str],
    build: impl Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    crate::markdown::update(path, sections, |name| {
        let body = build(name)?;
        oracles::public_text::check(&format!("section `{name}`"), &body)?;
        Ok(body)
    })
}

pub(crate) fn per_byte(value: u64, bytes: usize) -> f64 {
    value as f64 / (bytes.max(1) as f64)
}

/// A cell: the formatted value, or why there is none.
pub(crate) fn show<T>(o: &Outcome<T>, f: impl Fn(&T) -> String) -> String {
    match o {
        Outcome::Ok(v) => f(v),
        Outcome::Rejected => "rejects".to_owned(),
        Outcome::Crashed(r) => format!("**crash** ({r})"),
        Outcome::TimedOut(t) => format!("timeout (> {t} s)"),
    }
}

pub(crate) fn names(prefix: &[&str], suffix: &str) -> Vec<String> {
    let mut cols: Vec<String> = prefix.iter().map(|s| (*s).to_owned()).collect();
    cols.extend(
        Parser::MEASURED
            .iter()
            .map(|p| format!("{}{suffix}", p.name())),
    );
    cols
}

/// `ours / best` over the comparators in `among`, lower-is-better metrics.
pub(crate) fn ratio(row: &[Outcome<f64>], among: &[Parser]) -> String {
    let get = |p: &Parser| {
        Parser::MEASURED
            .iter()
            .position(|m| m == p)
            .and_then(|i| row.get(i))
            .and_then(|o| o.clone().ok())
    };
    let ours = get(&Parser::OneYaml);
    let best = among.iter().filter_map(get).reduce(f64::min);
    match (ours, best) {
        (Some(o), Some(b)) if b > 0.0 => fmt_ratio(o / b),
        _ => "–".to_owned(),
    }
}

/// `x` as a ratio with two significant digits below 1 (so `0.0012×`, not
/// `0.00×`), two decimals up to 100, whole numbers above.
pub(crate) fn fmt_ratio(x: f64) -> String {
    if x >= 100.0 {
        format!("{x:.0}×")
    } else if x >= 1.0 || x <= 0.0 {
        format!("{x:.2}×")
    } else {
        // Decimals so that two significant digits show; x in (0, 1).
        let decimals = (1.0 - x.log10().floor()).clamp(2.0, 8.0) as usize;
        format!("{x:.decimals$}×")
    }
}

type Grid<T> = Vec<Vec<Outcome<T>>>;

/// Measures every `(case, scale)` row with every parser in `parsers`, in
/// parallel, returning one row of outcomes per input row.
fn grid<T: Send>(
    rows: &[(&'static Case, usize)],
    parsers: &[Parser],
    jobs: usize,
    measure: impl Fn(Parser, &Case, usize) -> Result<Outcome<T>, String> + Sync,
) -> Result<Grid<T>, String> {
    let work: Vec<(&Case, usize, Parser)> = rows
        .iter()
        .flat_map(|(c, k)| parsers.iter().map(move |p| (*c, *k, *p)))
        .collect();
    let flat = parallel(&work, jobs, |(c, k, p)| measure(*p, c, *k));
    let mut out = Vec::with_capacity(rows.len());
    let mut it = flat.into_iter();
    for _ in rows {
        let row: Result<Vec<_>, String> = it.by_ref().take(parsers.len()).collect();
        out.push(row?);
    }
    Ok(out)
}

fn main_rows() -> Vec<(&'static Case, usize)> {
    CASES.iter().map(|c| (c, main_scale(c))).collect()
}

fn corpus_table() -> String {
    let mut s = format!(
        "`size_of::<oneyaml_syntax::Event>()` = {} bytes.\n\n",
        std::mem::size_of::<oneyaml_syntax::Event<'static>>()
    );
    s.push_str(&header(
        &[
            "Case",
            "Kind",
            "Scale",
            "Bytes",
            "FNV-1a 64",
            "oneyaml events (with trivia)",
            "saphyr events (YAML only)",
            "Contents",
        ]
        .map(str::to_owned),
    ));
    for c in CASES {
        let scale = main_scale(c);
        let input = c.generate(scale);
        let ours = measured_drain(Parser::OneYaml, &input);
        // saphyr is an iterative state machine with a flow-depth limit, so
        // it is safe to run in-process on every input.
        let theirs = measured_drain(Parser::Saphyr, &input);
        let theirs = if theirs.rejected {
            "rejects".to_owned()
        } else {
            thousands(theirs.events as u64)
        };
        let kind = if c.kind == Kind::RealWorld {
            "real-world"
        } else {
            "stress"
        };
        let _ = writeln!(
            s,
            "| {} | {kind} | {scale}× | {} | `{:016x}` | {} | {theirs} | {} |",
            c.name,
            thousands(input.len() as u64),
            fnv1a(input.as_bytes()),
            thousands(ours.events as u64),
            c.about
        );
    }
    s
}

fn instructions_table(env: &Env) -> Result<String, String> {
    let rows = main_rows();
    let g = grid(&rows, Parser::MEASURED, env.jobs, |p, c, k| {
        valgrind::instructions(&env.valgrind, &env.exe, p.name(), c.name, k, env.timeout)
    })?;
    let mut cols = names(&["Case", "Bytes", "oneyaml Ir"], " Ir/B");
    cols.push("oneyaml ÷ fastest event parser".to_owned());
    cols.push("oneyaml ÷ yaml-edit".to_owned());
    let mut s = header(&cols);
    for ((c, k), row) in rows.iter().zip(&g) {
        let bytes = c.generate(*k).len();
        let ours = row
            .first()
            .map_or_else(String::new, |o| show(o, |ir| thousands(*ir)));
        let rates: Vec<Outcome<f64>> = row
            .iter()
            .map(|o| o.clone().map(|ir| per_byte(ir, bytes)))
            .collect();
        let _ = write!(s, "| {} | {} | {ours} |", c.name, thousands(bytes as u64));
        for r in &rates {
            let _ = write!(s, " {} |", show(r, |v| format!("{v:.1}")));
        }
        let _ = writeln!(
            s,
            " {} | {} |",
            ratio(&rates, Parser::EVENT_COMPARATORS),
            ratio(&rates, &[Parser::YamlEdit])
        );
    }
    Ok(s)
}

/// One allocation sub-table: title, value from `(parser run, baseline run,
/// input bytes)`, cell format, whether to show the baseline column, and the
/// ratio column's title and comparators.
struct HeapTable {
    title: &'static str,
    value: fn(Heap, Heap, usize) -> f64,
    format: fn(f64) -> String,
    with_baseline: bool,
    ratio: (&'static str, &'static [Parser]),
}

const HEAP_TABLES: [HeapTable; 3] = [
    HeapTable {
        title: "Allocation count (blocks, `realloc` included; baseline subtracted)",
        value: |h, base, _| h.minus(base).total_blocks as f64,
        format: |x| thousands(x as u64),
        with_baseline: false,
        ratio: ("oneyaml ÷ fewest event parser", Parser::EVENT_COMPARATORS),
    },
    HeapTable {
        title: "Bytes allocated ÷ input bytes (baseline subtracted)",
        value: |h, base, b| per_byte(h.minus(base).total_bytes, b),
        format: |x| format!("{x:.2}"),
        with_baseline: false,
        ratio: ("oneyaml ÷ fewest event parser", Parser::EVENT_COMPARATORS),
    },
    HeapTable {
        title: "Whole-process peak heap ÷ input bytes (the baseline column is input generation alone)",
        value: |h, _, b| per_byte(h.peak_bytes, b),
        format: |x| format!("{x:.2}"),
        with_baseline: true,
        ratio: ("oneyaml ÷ yaml-edit", &[Parser::YamlEdit]),
    },
];

fn allocations_table(env: &Env) -> Result<String, String> {
    let rows = main_rows();
    let mut parsers = vec![Parser::Baseline];
    parsers.extend_from_slice(Parser::MEASURED);
    let g = grid(&rows, &parsers, env.jobs, |p, c, k| {
        valgrind::heap(&env.valgrind, &env.exe, p.name(), c.name, k, env.timeout)
    })?;
    let mut s = String::new();
    for t in &HEAP_TABLES {
        let _ = writeln!(s, "**{}**\n", t.title);
        let prefix: &[&str] = if t.with_baseline {
            &["Case", "Bytes", "baseline"]
        } else {
            &["Case", "Bytes"]
        };
        let mut cols = names(prefix, "");
        cols.push(t.ratio.0.to_owned());
        s.push_str(&header(&cols));
        for ((c, k), row) in rows.iter().zip(&g) {
            let bytes = c.generate(*k).len();
            let Some(Outcome::Ok(base)) = row.first() else {
                return Err(format!("baseline probe failed for {}", c.name));
            };
            let vals: Vec<Outcome<f64>> = row
                .iter()
                .skip(1)
                .map(|o| o.clone().map(|h| (t.value)(h, *base, bytes)))
                .collect();
            let _ = write!(s, "| {} | {} |", c.name, thousands(bytes as u64));
            if t.with_baseline {
                let _ = write!(s, " {} |", (t.format)(per_byte(base.peak_bytes, bytes)));
            }
            for v in &vals {
                let _ = write!(s, " {} |", show(v, |x| (t.format)(*x)));
            }
            let _ = writeln!(s, " {} |", ratio(&vals, t.ratio.1));
        }
        s.push('\n');
    }
    Ok(s)
}

/// What collecting the stream costs: DHAT peak heap ÷ input, net of the
/// input-generation baseline, for the pull parser dropping each event and
/// for `parse_with` keeping them all.
fn collect_table(env: &Env) -> Result<String, String> {
    let rows = main_rows();
    let parsers = [Parser::Baseline, Parser::OneYaml, Parser::OneYamlCollect];
    let g = grid(&rows, &parsers, env.jobs, |p, c, k| {
        valgrind::heap(&env.valgrind, &env.exe, p.name(), c.name, k, env.timeout)
    })?;
    let mut s = header(&[
        "Case".to_owned(),
        "Bytes".to_owned(),
        "pulled: peak ÷ input".to_owned(),
        "collected: peak ÷ input".to_owned(),
        "collected: bytes allocated ÷ input".to_owned(),
    ]);
    for ((c, k), row) in rows.iter().zip(&g) {
        let bytes = c.generate(*k).len();
        let (Some(Outcome::Ok(base)), Some(pulled), Some(collected)) =
            (row.first(), row.get(1), row.get(2))
        else {
            return Err(format!("baseline probe failed for {}", c.name));
        };
        let peak = |o: &Outcome<Heap>| {
            show(o, |h| {
                format!("{:.2}", per_byte(h.minus(*base).peak_bytes, bytes))
            })
        };
        let total = show(collected, |h| {
            format!("{:.2}", per_byte(h.minus(*base).total_bytes, bytes))
        });
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} |",
            c.name,
            thousands(bytes as u64),
            peak(pulled),
            peak(collected),
            total
        );
    }
    Ok(s)
}

/// What the lossless syntax tree costs (`Document` ≤ 8× input):
/// DHAT peak heap while building and walking it, and the heap a built
/// tree retains (a leaked tree's end-of-run figure), both ÷ input and net
/// of the input-generation baseline; and callgrind Ir/byte of building it
/// against draining the pull parser.
fn tree_table(env: &Env) -> Result<String, String> {
    let rows = main_rows();
    let heap_parsers = [
        Parser::Baseline,
        Parser::OneYamlTree,
        Parser::OneYamlTreeRetained,
    ];
    let heaps = grid(&rows, &heap_parsers, env.jobs, |p, c, k| {
        valgrind::heap(&env.valgrind, &env.exe, p.name(), c.name, k, env.timeout)
    })?;
    let ir_parsers = [Parser::OneYaml, Parser::OneYamlTree];
    let irs = grid(&rows, &ir_parsers, env.jobs, |p, c, k| {
        valgrind::instructions(&env.valgrind, &env.exe, p.name(), c.name, k, env.timeout)
    })?;
    let mut s = header(&[
        "Case".to_owned(),
        "Bytes".to_owned(),
        "tree elements".to_owned(),
        "elements ÷ byte".to_owned(),
        "peak ÷ input".to_owned(),
        "retained ÷ input".to_owned(),
        "stream Ir/B".to_owned(),
        "tree Ir/B".to_owned(),
        "tree ÷ stream".to_owned(),
    ]);
    for (((c, k), heap), ir) in rows.iter().zip(&heaps).zip(&irs) {
        let input = c.generate(*k);
        let bytes = input.len();
        let elements = measured_drain(Parser::OneYamlTree, &input).events;
        let (Some(Outcome::Ok(base)), Some(peak), Some(kept)) =
            (heap.first(), heap.get(1), heap.get(2))
        else {
            return Err(format!("baseline probe failed for {}", c.name));
        };
        // The tree's peak comes after input generation, when the heap holds
        // the input (the baseline's end-of-run bytes) plus the tree's
        // working set, so subtract those bytes, not the baseline's peak
        // (a generation transient that is gone by then). When the
        // whole-process peak is that transient, the tree's working set
        // stayed below it: shown as an upper bound.
        let peak = show(peak, |h| {
            if h.peak_bytes <= base.peak_bytes {
                let bound = base.peak_bytes.saturating_sub(base.end_bytes);
                format!("≤ {:.2}", per_byte(bound, bytes))
            } else {
                let net = h.peak_bytes.saturating_sub(base.end_bytes);
                format!("{:.2}", per_byte(net, bytes))
            }
        });
        let kept = show(kept, |h| {
            format!("{:.2}", per_byte(h.minus(*base).end_bytes, bytes))
        });
        let rate =
            |o: Option<&Outcome<u64>>| o.and_then(|o| o.clone().ok()).map(|ir| per_byte(ir, bytes));
        let (stream, tree) = (rate(ir.first()), rate(ir.get(1)));
        let cell = |r: Option<f64>| r.map_or_else(|| "–".to_owned(), |v| format!("{v:.1}"));
        let ratio = match (stream, tree) {
            (Some(a), Some(b)) if a > 0.0 => format!("{:.2}×", b / a),
            _ => "–".to_owned(),
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {:.2} | {peak} | {kept} | {} | {} | {ratio} |",
            c.name,
            thousands(bytes as u64),
            thousands(elements as u64),
            per_byte(elements as u64, bytes),
            cell(stream),
            cell(tree),
        );
    }
    Ok(s)
}

/// `(ir_k / bytes_k) / (ir_1 / bytes_1)`: 1.0 is perfectly linear.
#[must_use]
pub fn scaling_ratio(ir_1: u64, bytes_1: usize, ir_k: u64, bytes_k: usize) -> f64 {
    per_byte(ir_k, bytes_k) / per_byte(ir_1, bytes_1).max(f64::MIN_POSITIVE)
}

/// One scaling cell from the 1×/10×/100× outcomes of one parser: the
/// per-byte ratios that could be measured, then the first failure.
fn scaling_cell(runs: &[(Outcome<u64>, usize)]) -> String {
    let Some((Outcome::Ok(i1), b1)) = runs.first() else {
        return runs.first().map_or_else(
            || "–".to_owned(),
            |(o, _)| format!("{} at 1×", show(o, |_| String::new())),
        );
    };
    let mut parts = Vec::new();
    let mut superlinear = false;
    for ((o, b), k) in runs.iter().zip(SCALES).skip(1) {
        match o {
            Outcome::Ok(ir) => {
                let r = scaling_ratio(*i1, *b1, *ir, *b);
                superlinear |= r > SUPERLINEAR;
                parts.push(format!("{r:.2}"));
            }
            other => {
                parts.push(format!("{} at {k}×", show(other, |_| String::new())));
                break;
            }
        }
    }
    let flag = if superlinear { " **SUPERLINEAR**" } else { "" };
    format!("{}{flag}", parts.join(" / "))
}

fn scaling_table(env: &Env) -> Result<String, String> {
    let shapes: Vec<&'static Case> = CASES.iter().filter(|c| c.kind == Kind::Stress).collect();
    let rows: Vec<(&'static Case, usize)> = shapes
        .iter()
        .flat_map(|c| SCALES.map(|k| (*c, k)))
        .collect();
    let g = grid(&rows, Parser::MEASURED, env.jobs, |p, c, k| {
        valgrind::instructions(&env.valgrind, &env.exe, p.name(), c.name, k, env.timeout)
    })?;
    let mut s = header(&names(
        &["Shape", "Bytes 1× / 10× / 100×", "oneyaml Ir at 100×"],
        " (10× / 100×)",
    ));
    for (i, c) in shapes.iter().enumerate() {
        let lens: Vec<usize> = SCALES.iter().map(|k| c.generate(*k).len()).collect();
        let block = g
            .get(i.saturating_mul(3)..i.saturating_mul(3).saturating_add(3))
            .unwrap_or_default();
        let ir100 = block
            .get(2)
            .and_then(|r| r.first())
            .map_or_else(String::new, |o| show(o, |v| thousands(*v)));
        let bytes: Vec<String> = lens.iter().map(|b| thousands(*b as u64)).collect();
        let _ = write!(s, "| {} | {} | {ir100} |", c.name, bytes.join(" / "));
        for p in 0..Parser::MEASURED.len() {
            let runs: Vec<(Outcome<u64>, usize)> = block
                .iter()
                .zip(&lens)
                .filter_map(|(row, b)| row.get(p).map(|o| (o.clone(), *b)))
                .collect();
            let _ = write!(s, " {} |", scaling_cell(&runs));
        }
        s.push('\n');
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    /// `bench update` never writes a section naming an internal process
    /// identifier: the file keeps its old body, and a plain body is written.
    #[test]
    fn update_refuses_sections_with_internal_identifiers() {
        // Under the target directory (`target/<profile>/deps/..`), not /tmp.
        let exe = std::env::current_exe().unwrap();
        let dir = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(format!("bench-report-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        let doc = "<!-- bench:x:begin -->\nold\n<!-- bench:x:end -->\n";
        std::fs::write(&path, doc).unwrap();
        for bad in ["| target Q4.4 ≤ 1.25× |\n", "`abc` + unit 49b\n"] {
            let err = update_with(&path, &["x"], |_| Ok(bad.to_owned())).unwrap_err();
            assert!(err.contains("section `x`"), "{err}");
            assert!(err.contains("internal process identifier"), "{err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), doc);
        }
        update_with(&path, &["x"], |_| Ok("| target ≤ 1.25× |\n".to_owned())).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "<!-- bench:x:begin -->\n| target ≤ 1.25× |\n<!-- bench:x:end -->\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_section_is_an_error() {
        let env = Env {
            valgrind: "valgrind".into(),
            exe: "x".into(),
            jobs: 1,
            timeout: Duration::from_secs(1),
            provenance: crate::readme::Provenance::default(),
        };
        assert!(
            section("bogus", &env)
                .unwrap_err()
                .contains("unknown section")
        );
    }

    /// Every section a module builds is listed (so `bench update` with no
    /// `--section` regenerates it), and every listed one is built.
    #[test]
    fn every_section_is_listed() {
        for name in crate::serde_report::SECTIONS
            .iter()
            .chain(crate::document::SECTIONS)
        {
            assert!(SECTIONS.contains(name), "{name}");
        }
        let mut listed = SECTIONS.to_vec();
        listed.sort_unstable();
        listed.dedup();
        assert_eq!(listed.len(), SECTIONS.len());
    }

    #[test]
    fn scaling_ratio_is_per_byte() {
        assert!((scaling_ratio(100, 10, 1000, 100) - 1.0).abs() < 1e-9);
        assert!((scaling_ratio(100, 10, 2000, 100) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn scaling_cells() {
        let ok = |ir, b| (Outcome::Ok(ir), b);
        assert_eq!(
            scaling_cell(&[ok(10, 1), ok(100, 10), ok(1000, 100)]),
            "1.00 / 1.00"
        );
        assert!(scaling_cell(&[ok(10, 1), ok(100, 10), ok(2000, 100)]).contains("SUPERLINEAR"));
        let rej = [ok(10, 1), (Outcome::Rejected, 10), ok(1, 100)];
        assert_eq!(scaling_cell(&rej), "rejects at 10×");
        let late = [ok(10, 1), ok(500, 10), (Outcome::TimedOut(9), 100)];
        assert_eq!(
            scaling_cell(&late),
            "5.00 / timeout (> 9 s) at 100× **SUPERLINEAR**"
        );
        assert_eq!(scaling_cell(&[]), "–");
        let crash = [(Outcome::Crashed("stack overflow".into()), 1)];
        assert_eq!(scaling_cell(&crash), "**crash** (stack overflow) at 1×");
    }

    #[test]
    fn ratios_use_the_best_comparator() {
        // MEASURED order: oneyaml, saphyr, yaml-rust2, libyaml, yaml-edit.
        let row = [
            Outcome::Ok(6.0),
            Outcome::Ok(3.0),
            Outcome::Ok(2.0),
            Outcome::Rejected,
            Outcome::Ok(12.0),
        ];
        assert_eq!(ratio(&row, Parser::EVENT_COMPARATORS), "3.00×");
        assert_eq!(ratio(&row, &[Parser::YamlEdit]), "0.50×");
        assert_eq!(ratio(&row, &[Parser::LibYaml]), "–");
    }

    #[test]
    fn ratio_formatting() {
        assert_eq!(fmt_ratio(250.4), "250×");
        assert_eq!(fmt_ratio(3.0), "3.00×");
        assert_eq!(fmt_ratio(0.93), "0.93×");
        assert_eq!(fmt_ratio(0.0123), "0.012×");
        assert_eq!(fmt_ratio(0.000_021), "0.000021×");
    }
}
