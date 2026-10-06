//! The `document` and `edits` sections of `docs/performance.md` (the
//! `Document` parse, emit and edits at parity with yaml-edit; local edits
//! never O(document); `Document` retained ≤ 8× input on the real-world
//! corpus ≥ 100 KB).
//!
//! Probes are `doc:<lib>:<op>`, `<lib>` ∈ {`oneyaml`, `yaml-edit`}:
//!
//! * `parse`: oneyaml `Stream::parse_with(input, bench_settings())` (the
//!   strict entry point: tree plus strict-error check), yaml-edit
//!   `YamlFile::parse` (rowan CST); then dropped. No walk on either side.
//! * `emit`: `to_string()` of the parsed, untouched stream/file (parsing is
//!   preparation, not measured).
//! * `script`: the fixed edit script on the first document's root mapping
//!   (insert a new last entry `bench_edit_key: x`, replace its value with
//!   `'y'`, remove it), on a parsed tree (preparation). oneyaml:
//!   `Stream::document_mut(0)`, `insert`, `set_path`, `remove`; yaml-edit:
//!   `Document::set` twice, `Document::remove`
//!   (`oracles::yaml_edit::edit_script`).
//! * `round`: parse + script + `to_string`, from the input text.
//! * `retained`: parse and leak, so DHAT's end-of-run figure is what the
//!   parsed stream/file keeps; `baseline`: generate the input only.
//!
//! [`measured_drain_doc`] is the measured function (callgrind toggle
//! `*measured_drain*`). Wall-clock runs time the same function in a child
//! (`bench time doc:…`), informational only.

use std::fmt::Write as _;
use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use oneyaml::{Stream, path};
use oracles::yaml_edit::{self, YamlFile};

use crate::bench_settings;
use crate::corpus::{CASES, Case, Kind};
use crate::markdown::{header, parallel, thousands};
use crate::report::{Env, fmt_ratio, per_byte, show};
use crate::valgrind::{self, Heap, Outcome};

/// What the wall-clock tables' numbers are.
const MS_UNITS: &str = "Median of ≥ 5 runs, in ms.";

/// The sections, in document order.
pub const SECTIONS: &[&str] = &["document", "edits"];

/// The key the script inserts (absent from every corpus shape).
pub const KEY: &str = "bench_edit_key";

/// The `Document` memory gate: retained ≤ this × input…
pub const RETAINED_GATE: f64 = 8.0;

/// …on the real-world inputs of at least this many bytes.
pub const GATED_BYTES: usize = 100 * 1024;

/// Whose `Document`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocLib {
    /// `oneyaml::Stream`.
    OneYaml,
    /// yaml-edit 0.3.2 (`YamlFile`, through `oracles`).
    YamlEdit,
}

impl DocLib {
    /// Both, ours first.
    pub const ALL: &'static [DocLib] = &[DocLib::OneYaml, DocLib::YamlEdit];

    /// Stable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            DocLib::OneYaml => "oneyaml",
            DocLib::YamlEdit => "yaml-edit",
        }
    }
}

/// What a probe does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocOp {
    /// Generate the input only.
    Baseline,
    /// Parse and drop.
    Parse,
    /// Parse and leak.
    Retained,
    /// Emit a parsed stream.
    Emit,
    /// The edit script on a parsed stream.
    Script,
    /// Parse + script + emit.
    Round,
}

impl DocOp {
    const ALL: [DocOp; 6] = [
        DocOp::Baseline,
        DocOp::Parse,
        DocOp::Retained,
        DocOp::Emit,
        DocOp::Script,
        DocOp::Round,
    ];

    /// Stable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            DocOp::Baseline => "baseline",
            DocOp::Parse => "parse",
            DocOp::Retained => "retained",
            DocOp::Emit => "emit",
            DocOp::Script => "script",
            DocOp::Round => "round",
        }
    }
}

/// One probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocProbe {
    /// Whose document.
    pub lib: DocLib,
    /// What it does.
    pub op: DocOp,
}

impl DocProbe {
    /// `doc:<lib>:<op>`.
    #[must_use]
    pub fn name(self) -> String {
        format!("doc:{}:{}", self.lib.name(), self.op.name())
    }

    /// Parses [`DocProbe::name`].
    #[must_use]
    pub fn parse(name: &str) -> Option<DocProbe> {
        let (lib, op) = name.strip_prefix("doc:")?.split_once(':')?;
        Some(DocProbe {
            lib: DocLib::ALL.iter().copied().find(|l| l.name() == lib)?,
            op: DocOp::ALL.iter().copied().find(|o| o.name() == op)?,
        })
    }
}

/// A parsed stream or file, held between the preparation and the measured
/// step.
#[derive(Debug)]
#[non_exhaustive]
pub enum Prepared {
    /// Nothing prepared.
    Nothing,
    /// Our stream.
    Ours(Box<Stream>),
    /// yaml-edit's file.
    Theirs(YamlFile),
}

fn parse_ours(input: &str) -> Result<Stream, String> {
    Stream::parse_with(input, &bench_settings()).map_err(|e| e.to_string())
}

fn parse_theirs(input: &str) -> Result<YamlFile, String> {
    yaml_edit::parse_checked(input).ok_or_else(|| "yaml-edit reports errors".to_owned())
}

/// Parses `input` for the steps that start from a parsed document.
///
/// # Errors
/// The parser rejected the input.
pub fn prepare(probe: DocProbe, input: &str) -> Result<Prepared, String> {
    if !matches!(probe.op, DocOp::Emit | DocOp::Script) {
        return Ok(Prepared::Nothing);
    }
    Ok(match probe.lib {
        DocLib::OneYaml => Prepared::Ours(Box::new(parse_ours(input)?)),
        DocLib::YamlEdit => Prepared::Theirs(parse_theirs(input)?),
    })
}

/// Our edit script on document 0 of `stream`.
///
/// # Errors
/// A step failed (the corpus root is always a mapping without `KEY`).
pub fn script(stream: &mut Stream) -> Result<(), String> {
    let e = |e: oneyaml::EditError| e.to_string();
    let mut doc = stream.document_mut(0).ok_or("no first document")?;
    doc.root_mut().map_err(e)?.insert(KEY, "x").map_err(e)?;
    doc.set_path(&path![KEY], "y").map_err(e)?;
    let removed = doc.root_mut().map_err(e)?.remove(KEY).map_err(e)?;
    drop(black_box(removed));
    Ok(())
}

fn their_script(file: &YamlFile) -> Result<(), String> {
    if yaml_edit::edit_script(file, KEY) {
        Ok(())
    } else {
        Err("yaml-edit's script did not apply".to_owned())
    }
}

/// Runs `probe` over `input` (with what [`prepare`] made); returns a size
/// (bytes emitted, or the input length).
///
/// `#[inline(never)]` and a name matching `*measured_drain*`: callgrind
/// counts only while this is on the stack.
///
/// # Errors
/// The library rejected the input or a script step failed.
#[inline(never)]
pub fn measured_drain_doc(
    probe: DocProbe,
    input: &str,
    prepared: &mut Prepared,
) -> Result<usize, String> {
    let input = black_box(input);
    let out = match (probe.lib, probe.op, prepared) {
        (_, DocOp::Baseline, _) => Ok(input.len()),
        (DocLib::OneYaml, DocOp::Parse, _) => parse_ours(input).map(|s| black_box(s).len()),
        (DocLib::YamlEdit, DocOp::Parse, _) => parse_theirs(input).map(|f| {
            drop(black_box(f));
            input.len()
        }),
        (DocLib::OneYaml, DocOp::Retained, _) => parse_ours(input).map(|s| {
            let n = s.len();
            std::mem::forget(black_box(s));
            n
        }),
        (DocLib::YamlEdit, DocOp::Retained, _) => parse_theirs(input).map(|f| {
            std::mem::forget(black_box(f));
            input.len()
        }),
        (_, DocOp::Emit, Prepared::Ours(s)) => Ok(black_box(s.to_string()).len()),
        (_, DocOp::Emit, Prepared::Theirs(f)) => Ok(black_box(f.to_string()).len()),
        (_, DocOp::Script, Prepared::Ours(s)) => script(s).map(|()| input.len()),
        (_, DocOp::Script, Prepared::Theirs(f)) => their_script(f).map(|()| input.len()),
        (DocLib::OneYaml, DocOp::Round, _) => round_ours(input).map(|o| black_box(o).len()),
        (DocLib::YamlEdit, DocOp::Round, _) => round_theirs(input).map(|o| black_box(o).len()),
        _ => Err("nothing prepared".to_owned()),
    };
    black_box(out)
}

/// Parse + script + emit, ours.
///
/// # Errors
/// As [`script`], or the input does not parse.
pub fn round_ours(input: &str) -> Result<String, String> {
    let mut stream = parse_ours(input)?;
    script(&mut stream)?;
    Ok(stream.to_string())
}

/// Parse + script + emit, yaml-edit.
///
/// # Errors
/// The input has errors or the script did not apply.
pub fn round_theirs(input: &str) -> Result<String, String> {
    let file = parse_theirs(input)?;
    their_script(&file)?;
    Ok(file.to_string())
}

/// Median wall-clock time of one [`measured_drain_doc`] call on `case` and
/// the sample count (the preparation runs once, outside the timing).
///
/// # Errors
/// As [`prepare`] and [`measured_drain_doc`].
pub fn wall_clock(
    probe: DocProbe,
    input: &str,
    budget: Duration,
) -> Result<(Duration, usize), String> {
    let mut prepared = prepare(probe, input)?;
    measured_drain_doc(probe, input, &mut prepared)?;
    let mut failed = None;
    let timing = crate::serde_report::median(budget, || {
        if let Err(e) = measured_drain_doc(probe, input, &mut prepared) {
            failed = Some(e);
        }
    });
    failed.map_or(Ok(timing), Err)
}

fn cases() -> Vec<&'static Case> {
    CASES.iter().filter(|c| c.kind == Kind::RealWorld).collect()
}

/// Builds one section, or `None` if `name` isn't one.
#[must_use]
pub fn section(name: &str, env: &Env) -> Option<Result<String, String>> {
    Some(match name {
        "document" => document_table(env),
        "edits" => edits_table(env),
        _ => return None,
    })
}

type Grid<T> = Vec<Vec<Outcome<T>>>;

/// Every `probe` on every case, `[case][probe]`.
fn grid<T: Send>(
    probes: &[DocProbe],
    jobs: usize,
    measure: impl Fn(&str, &Case) -> Result<Outcome<T>, String> + Sync,
) -> Result<Grid<T>, String> {
    let cs = cases();
    let work: Vec<(&Case, DocProbe)> = cs
        .iter()
        .flat_map(|c| probes.iter().map(move |p| (*c, *p)))
        .collect();
    let flat = parallel(&work, jobs, |(c, p)| measure(&p.name(), c));
    let mut it = flat.into_iter();
    let mut out = Vec::with_capacity(cs.len());
    for _ in &cs {
        let row: Result<Vec<_>, String> = it.by_ref().take(probes.len()).collect();
        out.push(row?);
    }
    Ok(out)
}

fn instructions(env: &Env, probes: &[DocProbe]) -> Result<Grid<u64>, String> {
    grid(probes, env.jobs, |p, c| {
        valgrind::instructions(&env.valgrind, &env.exe, p, c.name, 1, env.timeout)
    })
}

/// Wall-clock medians in ns, one timing child at a time.
fn walls(env: &Env, probes: &[DocProbe]) -> Result<Grid<f64>, String> {
    let limit = env.timeout.checked_div(4).unwrap_or_default();
    grid(probes, 1, |p, c| {
        let args = [c.name, "1", "300"];
        match valgrind::run_child(&env.exe, &["time"], Path::new(p), &args, limit)? {
            Outcome::Ok((stdout, _)) => stdout
                .split_whitespace()
                .next()
                .and_then(|w| w.parse::<f64>().ok())
                .map(Outcome::Ok)
                .ok_or_else(|| format!("bad `time` output: {stdout:?}")),
            other => Ok(other.map(|_| 0.0)),
        }
    })
}

fn get<T: Clone>(row: &[Outcome<T>], i: usize) -> Option<T> {
    row.get(i).and_then(|o| o.clone().ok())
}

fn cell<T>(row: &[Outcome<T>], i: usize, f: impl Fn(&T) -> String) -> String {
    row.get(i).map_or_else(|| "–".to_owned(), |o| show(o, f))
}

/// `a ÷ b` and the parity verdict (met when the printed ratio ≤ 1.00×).
fn parity(a: Option<f64>, b: Option<f64>) -> (String, String) {
    match (a, b) {
        (Some(a), Some(b)) if b > 0.0 => {
            let r = fmt_ratio(a / b);
            let shown: f64 = r.trim_end_matches('×').parse().unwrap_or(a / b);
            let v = if shown <= 1.0 { "met" } else { "**missed**" };
            (r, v.to_owned())
        }
        _ => ("–".to_owned(), "–".to_owned()),
    }
}

fn ratio(a: Option<f64>, b: Option<f64>) -> String {
    parity(a, b).0
}

const fn probe(lib: DocLib, op: DocOp) -> DocProbe {
    DocProbe { lib, op }
}

const OURS: DocLib = DocLib::OneYaml;
const THEIRS: DocLib = DocLib::YamlEdit;

fn ms(ns: &f64) -> String {
    format!("{:.3}", ns / 1e6)
}

fn document_table(env: &Env) -> Result<String, String> {
    let cs = cases();
    // Instructions: parse and emit, both libraries.
    let ir_probes = [
        probe(OURS, DocOp::Parse),
        probe(THEIRS, DocOp::Parse),
        probe(OURS, DocOp::Emit),
        probe(THEIRS, DocOp::Emit),
    ];
    let irs = instructions(env, &ir_probes)?;
    // Memory: the baseline, parse (allocations), retained, both libraries.
    let heap_probes = [
        probe(OURS, DocOp::Baseline),
        probe(OURS, DocOp::Parse),
        probe(THEIRS, DocOp::Parse),
        probe(OURS, DocOp::Retained),
        probe(THEIRS, DocOp::Retained),
    ];
    let heaps = grid(&heap_probes, env.jobs, |p, c| {
        valgrind::heap(&env.valgrind, &env.exe, p, c.name, 1, env.timeout)
    })?;
    let wall_probes = [
        probe(OURS, DocOp::Parse),
        probe(THEIRS, DocOp::Parse),
        probe(OURS, DocOp::Emit),
        probe(THEIRS, DocOp::Emit),
    ];
    let wall = walls(env, &wall_probes)?;

    let mut s = String::from("**Instructions (Ir/B; lower is better)**\n\n");
    s.push_str(&header(
        &[
            "Case",
            "Bytes",
            "parse oneyaml",
            "parse yaml-edit",
            "parse ÷ yaml-edit",
            "parity",
            "emit oneyaml",
            "emit yaml-edit",
            "emit ÷ yaml-edit",
            "parity",
            "emit = input (oneyaml / yaml-edit)",
        ]
        .map(str::to_owned),
    ));
    for (c, row) in cs.iter().zip(&irs) {
        let input = c.generate(1);
        let bytes = input.len();
        let rate = |i: usize| get(row, i).map(|ir| per_byte(ir, bytes));
        let f = |ir: &u64| format!("{:.1}", per_byte(*ir, bytes));
        let (pr, pv) = parity(rate(0), rate(1));
        let (er, ev) = parity(rate(2), rate(3));
        // A byte-identical round trip for ours; yaml-edit's own round trip for
        // the record.
        let ours_same = parse_ours(&input).is_ok_and(|st| st.to_string() == input);
        let theirs_same = parse_theirs(&input).is_ok_and(|fl| fl.to_string() == input);
        let eq = |b: bool| if b { "=" } else { "≠" };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {pr} | {pv} | {} | {} | {er} | {ev} | {} / {} |",
            c.name,
            thousands(bytes as u64),
            cell(row, 0, f),
            cell(row, 1, f),
            cell(row, 2, f),
            cell(row, 3, f),
            eq(ours_same),
            eq(theirs_same),
        );
    }

    s.push_str("\n**Memory (DHAT; parse allocations net of the input-generation baseline; retained = the parsed stream/file kept, net of the baseline's end-of-run heap, including the parser's copy of the input where it keeps one)**\n\n");
    s.push_str(&header(
        &[
            "Case",
            "Bytes",
            "parse blocks oneyaml",
            "parse blocks yaml-edit",
            "retained ÷ input oneyaml",
            "retained ÷ input yaml-edit",
            "target: retained ≤ 8× (≥ 100 KB)",
        ]
        .map(str::to_owned),
    ));
    for (c, row) in cs.iter().zip(&heaps) {
        let bytes = c.generate(1).len();
        let Some(base) = get(row, 0) else {
            return Err(format!("document baseline probe failed for {}", c.name));
        };
        let blocks = |h: &Heap| thousands(h.minus(base).total_blocks);
        let kept = |h: &Heap| per_byte(h.end_bytes.saturating_sub(base.end_bytes), bytes);
        let ours_kept = get(row, 3).map(|h| kept(&h));
        let verdict = match ours_kept {
            _ if bytes < GATED_BYTES => "no target (< 100 KB)".to_owned(),
            Some(k) if k <= RETAINED_GATE => "met".to_owned(),
            Some(_) => "**missed**".to_owned(),
            None => "–".to_owned(),
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {verdict} |",
            c.name,
            thousands(bytes as u64),
            cell(row, 1, blocks),
            cell(row, 2, blocks),
            cell(row, 3, |h| format!("{:.2}", kept(h))),
            cell(row, 4, |h| format!("{:.2}", kept(h))),
        );
    }

    s.push('\n');
    s.push_str(&crate::wall::host_line_with(MS_UNITS));
    s.push_str("**Wall-clock (informational only)**\n\n");
    s.push_str(&header(
        &[
            "Case",
            "Bytes",
            "parse ms oneyaml",
            "parse ms yaml-edit",
            "parse ÷ yaml-edit",
            "emit ms oneyaml",
            "emit ms yaml-edit",
            "emit ÷ yaml-edit",
            "emit ÷ parse (oneyaml)",
        ]
        .map(str::to_owned),
    ));
    for (c, row) in cs.iter().zip(&wall) {
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.name,
            thousands(c.generate(1).len() as u64),
            cell(row, 0, ms),
            cell(row, 1, ms),
            ratio(get(row, 0), get(row, 1)),
            cell(row, 2, ms),
            cell(row, 3, ms),
            ratio(get(row, 2), get(row, 3)),
            ratio(get(row, 2), get(row, 0)),
        );
    }
    Ok(s)
}

fn edits_table(env: &Env) -> Result<String, String> {
    let cs = cases();
    let probes = [
        probe(OURS, DocOp::Script),
        probe(THEIRS, DocOp::Script),
        probe(OURS, DocOp::Round),
        probe(THEIRS, DocOp::Round),
    ];
    let irs = instructions(env, &probes)?;
    let wall = walls(env, &probes)?;
    let mut s = String::from("**Instructions (lower is better)**\n\n");
    s.push_str(&header(
        &[
            "Case",
            "Bytes",
            "script Ir oneyaml",
            "script Ir yaml-edit",
            "script ÷ yaml-edit",
            "parity",
            "round Ir/B oneyaml",
            "round Ir/B yaml-edit",
            "round ÷ yaml-edit",
            "parity",
            "output = input (oneyaml / yaml-edit)",
        ]
        .map(str::to_owned),
    ));
    for (c, row) in cs.iter().zip(&irs) {
        let input = c.generate(1);
        let bytes = input.len();
        let ir = |v: &u64| thousands(*v);
        let rate = |v: &u64| format!("{:.1}", per_byte(*v, bytes));
        let f = |i: usize| get(row, i).map(|v| v as f64);
        let (sr, sv) = parity(f(0), f(1));
        let (rr, rv) = parity(f(2), f(3));
        let same = |o: Result<String, String>| match o {
            Ok(t) if t == input => "=",
            Ok(_) => "≠",
            Err(_) => "–",
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {sr} | {sv} | {} | {} | {rr} | {rv} | {} / {} |",
            c.name,
            thousands(bytes as u64),
            cell(row, 0, ir),
            cell(row, 1, ir),
            cell(row, 2, rate),
            cell(row, 3, rate),
            same(round_ours(&input)),
            same(round_theirs(&input)),
        );
    }
    s.push('\n');
    s.push_str(&crate::wall::host_line_with(MS_UNITS));
    s.push_str("**Wall-clock (informational only)**\n\n");
    s.push_str(&header(
        &[
            "Case",
            "Bytes",
            "script ms oneyaml",
            "script ms yaml-edit",
            "script ÷ yaml-edit",
            "round ms oneyaml",
            "round ms yaml-edit",
            "round ÷ yaml-edit",
        ]
        .map(str::to_owned),
    ));
    for (c, row) in cs.iter().zip(&wall) {
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            c.name,
            thousands(c.generate(1).len() as u64),
            cell(row, 0, ms),
            cell(row, 1, ms),
            ratio(get(row, 0), get(row, 1)),
            cell(row, 2, ms),
            cell(row, 3, ms),
            ratio(get(row, 2), get(row, 3)),
        );
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use crate::corpus;

    #[test]
    fn probe_names_round_trip() {
        for lib in DocLib::ALL {
            for op in DocOp::ALL {
                let p = probe(*lib, op);
                assert_eq!(DocProbe::parse(&p.name()), Some(p));
            }
        }
        assert_eq!(DocProbe::parse("doc:oneyaml"), None);
        assert_eq!(DocProbe::parse("doc:x:parse"), None);
        assert_eq!(DocProbe::parse("doc:oneyaml:x"), None);
        assert_eq!(DocProbe::parse("serde:oneyaml:value"), None);
    }

    /// Every probe runs on a small real-world case; the script leaves the
    /// text as it found it; emit is byte-identical.
    #[test]
    fn every_probe_runs() {
        let input = corpus::find("compose-merge").unwrap().generate(1);
        for lib in DocLib::ALL {
            for op in DocOp::ALL {
                let p = probe(*lib, op);
                let mut prep = prepare(p, &input).unwrap();
                let n = measured_drain_doc(p, &input, &mut prep).unwrap();
                assert!(n > 0, "{}", p.name());
                if op == DocOp::Emit || op == DocOp::Round {
                    assert_eq!(n, input.len(), "{}", p.name());
                }
            }
        }
        assert_eq!(round_ours(&input).unwrap(), input);
        let mut stream = parse_ours(&input).unwrap();
        script(&mut stream).unwrap();
        script(&mut stream).unwrap();
        assert_eq!(stream.to_string(), input);
        let (t, n) = wall_clock(probe(OURS, DocOp::Script), &input, Duration::ZERO).unwrap();
        assert!(n >= 5 && t > Duration::ZERO);
    }

    #[test]
    fn failures_are_errors() {
        let bad = "[a, b\n";
        for lib in DocLib::ALL {
            assert!(prepare(probe(*lib, DocOp::Emit), bad).is_err());
            let mut none = Prepared::Nothing;
            assert!(measured_drain_doc(probe(*lib, DocOp::Parse), bad, &mut none).is_err());
            assert!(measured_drain_doc(probe(*lib, DocOp::Emit), "a: 1\n", &mut none).is_err());
            assert!(wall_clock(probe(*lib, DocOp::Round), bad, Duration::ZERO).is_err());
        }
        // The script needs a mapping root.
        let mut seq = parse_ours("- 1\n").unwrap();
        assert!(script(&mut seq).is_err());
        assert!(round_theirs("- 1\n").is_err());
    }

    #[test]
    fn parity_verdicts() {
        assert_eq!(parity(Some(1.0), Some(2.0)), ("0.50×".into(), "met".into()));
        assert_eq!(parity(Some(2.0), Some(1.0)).1, "**missed**");
        assert_eq!(parity(Some(1.004), Some(1.0)).1, "met");
        assert_eq!(parity(None, Some(1.0)), ("–".into(), "–".into()));
        assert_eq!(ratio(Some(3.0), Some(0.0)), "–");
    }
}
