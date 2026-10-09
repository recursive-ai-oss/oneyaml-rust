//! The benchmark summary, the README chart and the `environment` section.
//!
//! Three regions and four charts are **derived from
//! `docs/performance.md`**, never measured separately: [`generate`] reads
//! the generated tables of the `serde-instructions`, `ser-instructions`,
//! `document` and `edits` sections (and their wall-clock tables) plus the
//! `environment` section, and writes from exactly those cells
//! (`bench readme` measures nothing; `bench update` runs it after every
//! run), so nothing can disagree with the evidence:
//!
//! - the `bench:summary` region of `docs/performance.md`
//!   ([`summary_body`]): the detailed chart
//!   (`docs/img/bench-detail-{light,dark}.svg`, one labelled bar per row),
//!   one table per operation and the notes on the ratios, the wall-clock
//!   times and the run, with relative links only;
//! - the `bench:readme` region of `crates/oneyaml/README.md` and the
//!   `bench:chart` region of the repo-root `README.md` ([`readme_body`]):
//!   the compact chart (`docs/img/bench-summary-{light,dark}.svg`, see
//!   [`chart::compact_svg`]) and one link to the summary. With no table
//!   under it, the image's alt text carries what the chart shows
//!   ([`compact_alt_text`]): per operation, the range, how many workloads
//!   are slower and which.
//!
//! The headline figure is the owner's: "× faster than `<competitor>`" =
//! competitor instructions ÷ oneyaml instructions (higher is better), two
//! significant figures, computed from the two cells shown beside it.
//! Values under 1.0× are shown as they are and marked slower
//! (`0.80× — slower`). Each table also has a wall-clock "× faster" column
//! from the medians of the wall-clock tables, and a note under the tables
//! names every row where oneyaml is slower in wall-clock time.
//!
//! The detailed chart's alt text is a short summary pointing at the tables
//! under it; both charts' own `<desc>` lists every bar
//! ([`chart_description`]).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::chart::{self, Bar, Group, Panel, Tone};
use crate::markdown::replace_section;

/// The section [`generate`] maintains in the crate README.
pub const README_SECTION: &str = "readme";

/// The section [`generate`] maintains in the repo-root README.
pub const CHART_SECTION: &str = "chart";

/// The `docs/performance.md` section that records the run's provenance.
pub const ENVIRONMENT_SECTION: &str = "environment";

/// Where the repository lives; the crate README is also the docs.rs crate
/// root, so every link in the README regions is absolute (the summary
/// region in `docs/performance.md` links relatively). The workspace `repository`
/// field, through [`oracles::readme::REPO`] (one source of truth).
pub use oracles::readme::REPO;

/// Raw-file base for the chart, so the same URL works from the repo-root
/// README, the crate README, docs.rs and crates.io; derived from [`REPO`]
/// ([`oracles::readme::raw_base`]). The git ref is an input of
/// [`generate`]: [`DEFAULT_REF`] day to day; at release, `bench pin-links`
/// ([`crate::pin`]) repoints it to the tag, so a published README shows the
/// chart of its own tables.
#[must_use]
pub fn raw() -> String {
    oracles::readme::raw_base()
}

/// The git ref the README's chart and links point at unless told otherwise.
pub const DEFAULT_REF: &str = "main";

/// The `docs/performance.md` sections the summary reads (besides
/// `environment`). `bench update` re-records `environment` whenever it
/// re-measures one of them ([`with_environment`]).
pub const README_SOURCES: [&str; 6] = [
    "serde-instructions",
    "serde-wall",
    "ser-instructions",
    "ser-wall",
    "document",
    "edits",
];

/// `names` with [`ENVIRONMENT_SECTION`] first when any of them is one the
/// summary reads, so the recorded provenance never lags the numbers.
#[must_use]
pub fn with_environment<'a>(names: &[&'a str]) -> Vec<&'a str> {
    let mut out: Vec<&'a str> = names.to_vec();
    let measures_source = names.iter().any(|n| README_SOURCES.contains(n));
    if measures_source && !names.contains(&ENVIRONMENT_SECTION) {
        out.insert(0, ENVIRONMENT_SECTION);
    }
    out
}

/// The `docs/performance.md` section [`generate`] fills with the summary
/// tables, the detailed chart and the notes.
pub const SUMMARY_SECTION: &str = "summary";

/// The compact README chart, relative to the workspace root.
pub const CHART_LIGHT: &str = "docs/img/bench-summary-light.svg";
/// The dark-mode compact README chart, relative to the workspace root.
pub const CHART_DARK: &str = "docs/img/bench-summary-dark.svg";

/// The detailed chart (one labelled bar per summary row) shown with the
/// summary tables, relative to the workspace root.
pub const DETAIL_LIGHT: &str = "docs/img/bench-detail-light.svg";
/// The dark-mode detailed chart, relative to the workspace root.
pub const DETAIL_DARK: &str = "docs/img/bench-detail-dark.svg";

/// A file under `docs/` as `docs/performance.md` links to it: relative,
/// so the summary region names no git ref and needs no pinning.
fn from_docs(path: &str) -> &str {
    path.strip_prefix("docs/").unwrap_or(path)
}

/// The README, relative to the workspace root.
pub const README: &str = "crates/oneyaml/README.md";

/// The repo-root README, relative to the workspace root.
pub const ROOT_README: &str = "README.md";

/// The evidence file, relative to the workspace root.
pub const PERFORMANCE: &str = "docs/performance.md";

/// The workloads of the summary: the real-world-shaped corpus cases from
/// a few kB to 1 MB, one per ecosystem (GitHub Actions, docker-compose
/// with merge keys, Helm values with long comment runs, OpenAPI,
/// multi-document Kubernetes). `k8s-manifest` (a subset of `k8s-multi`)
/// and `openapi-10m` (the same shape as `openapi-100k`) stay in the full
/// tables.
pub const WORKLOADS: [&str; 5] = [
    "gha-workflow",
    "compose-merge",
    "helm-values",
    "openapi-100k",
    "k8s-multi",
];

/// The comparator crates whose versions the summary's Measured line names.
pub const COMPARATORS: [&str; 9] = [
    "serde_yaml_ng",
    "serde_norway",
    "serde-saphyr",
    "serde_yaml2",
    "serde_json",
    "yaml-edit",
    "saphyr-parser",
    "yaml-rust2",
    "unsafe-libyaml",
];

/// The workspace root this binary was built from.
#[must_use]
pub fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

// ---------------------------------------------------------------------------
// The `environment` section.
// ---------------------------------------------------------------------------

/// Overrides for the provenance the `environment` section records.
///
/// Only for re-recording an earlier run's provenance (the run's revision
/// and date are known, the tables are not re-measured); a normal `update`
/// leaves both `None` and detects them.
#[derive(Clone, Debug, Default)]
pub struct Provenance {
    /// The measured revision, as written (e.g. ``"`4c6d70c`"``). It is
    /// published in `docs/performance.md`, so it must read as plain text
    /// ([`oracles::public_text`]); how the measured tree differed from the
    /// commit goes in [`revision_note`].
    ///
    /// [`revision_note`]: Provenance::revision_note
    pub revision: Option<String>,
    /// A reader-facing note on how the measured tree differed from the
    /// commit in [`revision`](Provenance::revision) (e.g. "only the
    /// benchmark harness and docs differed"), recorded as the `Revision
    /// note` row and shown in the summary as `` `commit` (note) ``.
    pub revision_note: Option<String>,
    /// The measurement date (`YYYY-MM-DD`, UTC).
    pub date: Option<String>,
}

/// The `environment` section: who measured what, where.
///
/// # Errors
/// A given `--revision` or `--revision-note` that names an internal process
/// identifier ([`oracles::public_text`]): the section is published.
/// Detection failures are not errors; they become `unknown` cells.
pub fn environment_section(p: &Provenance) -> Result<String, String> {
    for (flag, given) in [
        ("--revision", p.revision.as_deref()),
        ("--revision-note", p.revision_note.as_deref()),
    ] {
        if let Some(text) = given {
            oracles::public_text::check(&format!("the recorded {flag}"), text)?;
        }
    }
    let root = workspace_root();
    let revision = p.revision.clone().unwrap_or_else(|| detect_revision(&root));
    let date = p.date.clone().unwrap_or_else(today_utc);
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap_or_default();
    let versions: Vec<String> = COMPARATORS
        .iter()
        .map(|name| match comparator_version(&lock, name) {
            Some(v) => format!("{name} {v}"),
            None => format!("{name} unknown"),
        })
        .collect();
    let note = p.revision_note.clone().map(|n| ("Revision note", n));
    let rows = [("Revision", revision)].into_iter().chain(note).chain([
        ("Date", date),
        ("CPU", detect_cpu()),
        (
            "OS",
            format!("{} ({})", detect_os(), std::env::consts::ARCH),
        ),
        ("rustc", command_line("rustc", &["-V"])),
        ("Build", crate::build::description()),
        ("valgrind", command_line("valgrind", &["--version"])),
        ("Comparators", versions.join(", ")),
    ]);
    let mut s = String::from("| Measured | |\n|---|---|\n");
    for (k, v) in rows {
        let _ = writeln!(s, "| {k} | {} |", v.replace('|', "/"));
    }
    Ok(s)
}

fn detect_revision(root: &Path) -> String {
    let head = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let Some(head) = head else {
        return "unknown".to_owned();
    };
    let dirty = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "--", "crates", "Cargo.lock"])
        .output()
        .ok()
        .is_some_and(|o| !o.stdout.is_empty());
    if dirty {
        format!("`{head}` + uncommitted changes")
    } else {
        format!("`{head}`")
    }
}

fn command_line(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|l| l.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn detect_cpu() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn detect_os() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| std::env::consts::OS.to_owned())
}

/// The version `Cargo.lock` records for package `name` (the first entry).
#[must_use]
pub fn lock_version(lock: &str, name: &str) -> Option<String> {
    let needle = format!("name = \"{name}\"");
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == needle {
            return lines
                .next()
                .and_then(|l| l.trim().strip_prefix("version = "))
                .map(|v| v.trim_matches('"').to_owned());
        }
    }
    None
}

/// The version of comparator `name` the `oracles` crate (the only crate
/// that depends on comparators) is locked to: the version its
/// `Cargo.lock` dependency entry names when several versions are locked,
/// else the only locked one.
#[must_use]
pub fn comparator_version(lock: &str, name: &str) -> Option<String> {
    let mut in_oracles = false;
    let mut in_deps = false;
    let prefix = format!("\"{name} ");
    for line in lock.lines() {
        let t = line.trim();
        if t == "[[package]]" {
            in_oracles = false;
            in_deps = false;
        } else if t == "name = \"oracles\"" {
            in_oracles = true;
        } else if in_oracles && t == "dependencies = [" {
            in_deps = true;
        } else if in_deps && t == "]" {
            break;
        } else if in_deps && let Some(v) = t.strip_prefix(&prefix) {
            return Some(v.trim_end_matches(',').trim_end_matches('"').to_owned());
        }
    }
    lock_version(lock, name)
}

fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe
        .saturating_sub(doe / 1460)
        .saturating_add(doe / 36_524)
        .saturating_sub(doe / 146_096))
        / 365;
    let doy = doe.saturating_sub(
        yoe.saturating_mul(365)
            .saturating_add(yoe / 4)
            .saturating_sub(yoe / 100),
    );
    let mp = (doy.saturating_mul(5).saturating_add(2)) / 153;
    let d = doy
        .saturating_sub((mp.saturating_mul(153).saturating_add(2)) / 5)
        .saturating_add(1);
    let m = if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    };
    let y = yoe
        .saturating_add(era.saturating_mul(400))
        .saturating_add(i64::from(m <= 2));
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

// ---------------------------------------------------------------------------
// Reading the generated tables back.
// ---------------------------------------------------------------------------

/// One markdown table: its header and its rows of cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// Column names.
    pub header: Vec<String>,
    /// Data rows (the `|---|` separator is not a row).
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// The index of column `name`.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }

    /// The first row whose cells match every `(column, value)` pair.
    #[must_use]
    pub fn row(&self, keys: &[(&str, &str)]) -> Option<&[String]> {
        let idx: Option<Vec<(usize, &str)>> = keys
            .iter()
            .map(|(c, v)| self.column(c).map(|i| (i, *v)))
            .collect();
        let idx = idx?;
        self.rows
            .iter()
            .find(|r| {
                idx.iter()
                    .all(|(i, v)| r.get(*i).is_some_and(|c| c.as_str() == *v))
            })
            .map(Vec::as_slice)
    }

    /// The cell in `row` under column `name`.
    #[must_use]
    pub fn cell<'a>(&self, row: &'a [String], name: &str) -> Option<&'a str> {
        self.column(name)
            .and_then(|i| row.get(i))
            .map(String::as_str)
    }
}

/// The body of section `name` of `doc` (between its markers).
///
/// # Errors
/// If the markers are missing or out of order.
pub fn section_body<'a>(doc: &'a str, name: &str) -> Result<&'a str, String> {
    let begin = format!("<!-- bench:{name}:begin -->\n");
    let end = format!("<!-- bench:{name}:end -->");
    let b = doc
        .find(&begin)
        .map(|b| b.saturating_add(begin.len()))
        .ok_or_else(|| format!("missing marker {begin:?}"))?;
    let rest = doc.get(b..).unwrap_or_default();
    let e = rest
        .find(&end)
        .ok_or_else(|| format!("missing marker {end:?}"))?;
    Ok(rest.get(..e).unwrap_or_default())
}

/// Every markdown table in `text`, in order.
#[must_use]
pub fn tables(text: &str) -> Vec<Table> {
    let mut out = Vec::new();
    let mut current: Option<Table> = None;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            if let Some(t) = current.take() {
                out.push(t);
            }
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_owned())
            .collect();
        match current.as_mut() {
            None => {
                current = Some(Table {
                    header: cells,
                    rows: Vec::new(),
                });
            }
            Some(t) => {
                if !cells.iter().all(|c| c.chars().all(|ch| ch == '-')) {
                    t.rows.push(cells);
                }
            }
        }
    }
    if let Some(t) = current {
        out.push(t);
    }
    out
}

/// A numeric cell: `1,058,925`, `**3.57×**` and `414.8` read as numbers;
/// `n/a`, `rejects`, `differs`, `timeout …` do not.
#[must_use]
pub fn number(cell: &str) -> Option<f64> {
    let cleaned: String = cell
        .chars()
        .filter(|c| !matches!(c, ',' | '*' | '×'))
        .collect();
    cleaned.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `x` to two significant figures (`1.134` → `1.1`, `0.2824` → `0.28`,
/// `12.3` → `12`, `126` → `130`).
#[must_use]
pub fn sig2(x: f64) -> String {
    if !(x.is_finite() && x > 0.0) {
        return "0".to_owned();
    }
    let digits = |v: f64| 1_i32.saturating_sub(v.log10().floor() as i32);
    let mut p = digits(x);
    let scale = |p: i32| 10_f64.powi(p);
    let mut r = (x * scale(p)).round() / scale(p);
    // Rounding can carry into a new digit (9.96 → 10.0): re-round there.
    let q = digits(r);
    if q != p {
        p = q;
        r = (x * scale(p)).round() / scale(p);
    }
    let decimals = usize::try_from(p.max(0)).unwrap_or(0);
    format!("{r:.decimals$}")
}

// ---------------------------------------------------------------------------
// The summary.
// ---------------------------------------------------------------------------

/// Which competitor a row compares with.
#[derive(Clone, Copy, Debug)]
enum Competitor {
    /// The comparator named in this column of the row (the serde tables'
    /// "fastest feature-complete": the one the serde speed ratio is gated on).
    Column(&'static str),
    /// Always this comparator.
    Fixed(&'static str),
}

/// How to read one operation out of `docs/performance.md`.
#[derive(Clone, Copy, Debug)]
struct Op {
    /// Table heading in the summary.
    title: &'static str,
    /// Short name for the chart and alt text.
    short: &'static str,
    /// The group label of the compact README chart.
    compact: &'static str,
    /// The unit of the absolute columns.
    unit: &'static str,
    /// The `docs/performance.md` section.
    section: &'static str,
    /// Extra key column, e.g. `("Workload", "typed")`.
    filter: Option<(&'static str, &'static str)>,
    /// oneyaml's instructions column.
    ours: &'static str,
    competitor: Competitor,
    /// Who the competitor is, in words, for the charts' description and alt
    /// texts.
    versus: &'static str,
    /// The competitor's instructions column; `{}` is the competitor.
    theirs: &'static str,
    /// The wall-clock section, oneyaml's column, the competitor's column
    /// (`{}` = competitor), and whether a higher value is faster.
    wall: (&'static str, &'static str, &'static str, bool),
}

const OPS: [Op; 4] = [
    Op {
        title: "serde read (typed)",
        short: "serde read",
        compact: "serde read",
        unit: "Ir/B",
        section: "serde-instructions",
        filter: Some(("Workload", "typed")),
        ours: "oneyaml Ir/B",
        competitor: Competitor::Column("fastest feature-complete"),
        versus: "the fastest feature-complete YAML serde library on each workload",
        theirs: "{} Ir/B",
        wall: ("serde-wall", "oneyaml MB/s", "{} MB/s", true),
    },
    Op {
        title: "serde write (typed)",
        short: "serde write",
        compact: "serde write",
        unit: "Ir/B",
        section: "ser-instructions",
        filter: Some(("Workload", "typed")),
        ours: "oneyaml Ir/B",
        competitor: Competitor::Column("fastest feature-complete"),
        versus: "the fastest feature-complete YAML serde library on each workload",
        theirs: "{} Ir/B",
        wall: ("ser-wall", "oneyaml MB/s", "{} MB/s", true),
    },
    Op {
        title: "`Document` parse",
        short: "Document parse",
        compact: "Document parse",
        unit: "Ir/B",
        section: "document",
        filter: None,
        ours: "parse oneyaml",
        competitor: Competitor::Fixed("yaml-edit"),
        versus: "yaml-edit",
        theirs: "parse {}",
        wall: ("document", "parse ms oneyaml", "parse ms {}", false),
    },
    Op {
        title: "`Document` edit script (insert + set + remove)",
        short: "Document edit script (insert + set + remove)",
        compact: "Document edit script",
        unit: "Ir per script",
        section: "edits",
        filter: None,
        ours: "script Ir oneyaml",
        competitor: Competitor::Fixed("yaml-edit"),
        versus: "yaml-edit",
        theirs: "script Ir {}",
        wall: ("edits", "script ms oneyaml", "script ms {}", false),
    },
];

/// Wall-clock direction relative to the instruction-count direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wall {
    /// Wall-clock points the same way.
    Agrees,
    /// Wall-clock points the other way.
    Differs,
    /// The wall-clock medians are equal as printed.
    Tie,
    /// No ratio, or a wall-clock cell that holds no number.
    Missing,
}

/// One summary row, every value read from `docs/performance.md`.
#[derive(Clone, Debug)]
pub struct Row {
    /// The workload (corpus case).
    pub workload: String,
    /// Its size in bytes, as printed.
    pub bytes: f64,
    /// The competitor.
    pub competitor: String,
    /// oneyaml's cell, as printed.
    pub ours: String,
    /// The competitor's cell, as printed.
    pub theirs: String,
    /// Competitor ÷ oneyaml, when both cells are numbers.
    pub faster: Option<f64>,
    /// Wall-clock direction.
    pub wall: Wall,
    /// The wall-clock "× faster" (competitor's median time ÷ oneyaml's),
    /// when both medians are numbers.
    pub wall_faster: Option<f64>,
}

/// One operation's rows.
#[derive(Clone, Debug)]
pub struct Section {
    /// Table heading in the summary.
    pub title: &'static str,
    /// Chart / alt-text name.
    pub short: &'static str,
    /// The group label of the compact README chart.
    pub compact: &'static str,
    /// Who the competitor is, in words.
    pub versus: &'static str,
    /// Unit of the absolute cells.
    pub unit: &'static str,
    /// The rows, in [`WORKLOADS`] order.
    pub rows: Vec<Row>,
}

fn find_table(doc: &str, section: &str, column: &str) -> Result<Table, String> {
    let body = section_body(doc, section)?;
    tables(body)
        .into_iter()
        .find(|t| t.column(column).is_some())
        .ok_or_else(|| format!("section `{section}` has no table with column `{column}`"))
}

/// Whether oneyaml is faster than `competitor` by wall-clock on this row:
/// [`Wall::Agrees`] = faster, [`Wall::Differs`] = slower (the caller
/// relates it to the instruction-count direction); [`Wall::Missing`] when
/// a cell is present but not a number (`rejects`, `timeout …`). With it,
/// the wall-clock "× faster": the competitor's median time ÷ oneyaml's
/// (oneyaml's throughput ÷ the competitor's, for MB/s columns).
///
/// # Errors
/// A missing wall-clock section, table, row or column.
fn wall_direction(
    doc: &str,
    op: &Op,
    keys: &[(&str, &str)],
    competitor: &str,
) -> Result<(Wall, Option<f64>), String> {
    let (section, ours, theirs, higher_faster) = op.wall;
    let theirs = theirs.replace("{}", competitor);
    let t = find_table(doc, section, ours)?;
    let row = t
        .row(keys)
        .ok_or_else(|| format!("section `{section}` has no row {keys:?}"))?;
    let cell = |col: &str| {
        t.cell(row, col)
            .ok_or_else(|| format!("section `{section}` has no column `{col}`"))
    };
    let (Some(o), Some(c)) = (number(cell(ours)?), number(cell(&theirs)?)) else {
        return Ok((Wall::Missing, None));
    };
    let ratio = match (higher_faster, o > 0.0, c > 0.0) {
        (true, _, true) => Some(o / c),
        (false, true, _) => Some(c / o),
        _ => None,
    };
    if (o - c).abs() < f64::EPSILON {
        return Ok((Wall::Tie, ratio));
    }
    // `true` when oneyaml is faster by wall-clock.
    let wall_faster = if higher_faster { o > c } else { o < c };
    Ok((
        if wall_faster {
            Wall::Agrees
        } else {
            Wall::Differs
        },
        ratio,
    ))
}

/// Reads the summary rows out of `docs/performance.md`'s text.
///
/// # Errors
/// A missing section, table, column or workload row, in the instruction
/// tables or their wall-clock tables, or a `Bytes` cell that is not a
/// number: the summary is never generated from partial evidence. Only a
/// measured cell that holds no number (`rejects`, `timeout …`) becomes
/// `n/a`.
pub fn summary(doc: &str) -> Result<Vec<Section>, String> {
    let mut out = Vec::new();
    for op in &OPS {
        let t = find_table(doc, op.section, op.ours)?;
        let mut rows = Vec::new();
        for w in WORKLOADS {
            let mut keys = vec![("Case", w)];
            if let Some(f) = op.filter {
                keys.push(f);
            }
            let row = t
                .row(&keys)
                .ok_or_else(|| format!("section `{}` has no row {keys:?}", op.section))?;
            let competitor = match op.competitor {
                Competitor::Fixed(c) => c.to_owned(),
                Competitor::Column(col) => t
                    .cell(row, col)
                    .ok_or_else(|| format!("section `{}` has no column `{col}`", op.section))?
                    .to_owned(),
            };
            let theirs_col = op.theirs.replace("{}", &competitor);
            let ours = t
                .cell(row, op.ours)
                .ok_or_else(|| format!("no `{}` cell for {w}", op.ours))?
                .to_owned();
            let theirs = t
                .cell(row, &theirs_col)
                .ok_or_else(|| format!("no `{theirs_col}` cell for {w}"))?
                .to_owned();
            let faster = match (number(&ours), number(&theirs)) {
                (Some(o), Some(c)) if o > 0.0 => Some(c / o),
                _ => None,
            };
            let bytes = t
                .cell(row, "Bytes")
                .ok_or_else(|| format!("section `{}` has no column `Bytes`", op.section))?;
            let bytes = number(bytes).ok_or_else(|| {
                format!("section `{}`: `Bytes` of {w} is not a number", op.section)
            })?;
            // Always read, so missing wall-clock evidence is an error even
            // where the instructions are not measured.
            let (by_wall, wall_faster) = wall_direction(doc, op, &keys, &competitor)?;
            let wall = match faster {
                None => Wall::Missing,
                Some(f) => {
                    // `wall_direction` answers "is oneyaml faster by
                    // wall-clock"; flip it when instructions say slower.
                    match (by_wall, f < 1.0) {
                        (Wall::Agrees, true) => Wall::Differs,
                        (Wall::Differs, true) => Wall::Agrees,
                        (other, _) => other,
                    }
                }
            };
            rows.push(Row {
                workload: w.to_owned(),
                bytes,
                competitor,
                ours,
                theirs,
                faster,
                wall,
                wall_faster,
            });
        }
        out.push(Section {
            title: op.title,
            short: op.short,
            compact: op.compact,
            versus: op.versus,
            unit: op.unit,
            rows,
        });
    }
    Ok(out)
}

/// `3078` → `3.1 kB`, `1050323` → `1.1 MB` (SI, two significant figures).
#[must_use]
pub fn size(bytes: f64) -> String {
    if bytes >= 1e6 {
        format!("{} MB", sig2(bytes / 1e6))
    } else if bytes >= 1e3 {
        format!("{} kB", sig2(bytes / 1e3))
    } else {
        format!("{} B", sig2(bytes))
    }
}

/// A ratio in words: `1.1×`, `0.28× — slower` (the standard's form; never
/// inverted), or `not measured`. The tables, the alt text and the chart all
/// use it.
#[must_use]
pub fn ratio_text(f: Option<f64>) -> String {
    match f {
        None => "not measured".to_owned(),
        Some(f) if f < 1.0 => format!("{}× — slower", sig2(f)),
        Some(f) => format!("{}×", sig2(f)),
    }
}

/// The table cell: `**1.1×**`, `0.28× — slower`, or `n/a`.
#[must_use]
pub fn faster_cell(f: Option<f64>) -> String {
    match f {
        None => "n/a".to_owned(),
        Some(f) if f < 1.0 => ratio_text(Some(f)),
        Some(f) => format!("**{}**", ratio_text(Some(f))),
    }
}

/// The absolute cells, with the better (lower) value in bold; neither
/// when they are equal or either is not a number.
#[must_use]
pub fn bold_better(ours: &str, theirs: &str) -> (String, String) {
    let bold = |c: &str| format!("**{c}**");
    match (number(ours), number(theirs)) {
        (Some(o), Some(t)) if o < t => (bold(ours), theirs.to_owned()),
        (Some(o), Some(t)) if t < o => (ours.to_owned(), bold(theirs)),
        _ => (ours.to_owned(), theirs.to_owned()),
    }
}

/// The `environment` section as `(key, value)` pairs.
///
/// # Errors
/// If the section or its table is missing.
pub fn environment(doc: &str) -> Result<Vec<(String, String)>, String> {
    let body = section_body(doc, ENVIRONMENT_SECTION)?;
    let t = tables(body)
        .into_iter()
        .next()
        .ok_or_else(|| "the `environment` section has no table".to_owned())?;
    Ok(t.rows
        .iter()
        .filter_map(|r| match r.as_slice() {
            [k, v, ..] => Some((k.clone(), v.clone())),
            _ => None,
        })
        .collect())
}

fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Result<&'a str, String> {
    env.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("the `environment` section has no `{key}` row"))
}

/// The recorded `name version` entries of the competitors that appear in
/// `sections`, in the recorded order.
fn competitor_versions(recorded: &str, sections: &[Section]) -> String {
    recorded
        .split(", ")
        .filter(|entry| {
            let name = entry.split(' ').next().unwrap_or_default();
            sections
                .iter()
                .any(|sec| sec.rows.iter().any(|r| r.competitor == name))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The chart's description inside the SVG (`<desc>`): every bar in words.
#[must_use]
pub fn chart_description(sections: &[Section]) -> String {
    let mut s = format!(
        "Bar chart: how many times fewer CPU instructions oneyaml executes than each \
         competitor (competitor ÷ oneyaml; higher is better; 1.0× is equal), against {}.",
        basis(sections)
    );
    for sec in sections {
        let _ = write!(s, " {}:", sec.short);
        let parts: Vec<String> = sec
            .rows
            .iter()
            .map(|r| {
                format!(
                    " {} {} vs {}",
                    r.workload,
                    ratio_text(r.faster),
                    r.competitor
                )
            })
            .collect();
        s.push_str(&parts.join(","));
        s.push('.');
    }
    s
}

/// The image's alt text: a short summary of the chart (shown in place of
/// the image when it does not load), naming the rows where oneyaml is
/// slower and pointing at the tables, which carry every number.
#[must_use]
pub fn alt_text(sections: &[Section]) -> String {
    let total: usize = sections.iter().map(|sec| sec.rows.len()).sum();
    let workloads = sections.first().map_or(0, |sec| sec.rows.len());
    let mut s = format!(
        "Bar chart: how many times fewer CPU instructions oneyaml executes than each \
         competitor (higher is better), against {}, on {workloads} workloads.",
        basis(sections)
    );
    let slower = rows_matching(sections, |r| r.faster.is_some_and(|f| f < 1.0));
    let unmeasured = rows_matching(sections, |r| r.faster.is_none());
    let n_slower = count_rows(sections, |r| r.faster.is_some_and(|f| f < 1.0));
    if n_slower == 0 {
        s.push_str(" oneyaml executes no more instructions on any measured row.");
    } else {
        let _ = write!(
            s,
            " oneyaml executes more instructions on {n_slower} of {total} rows: {slower}."
        );
    }
    if !unmeasured.is_empty() {
        let _ = write!(s, " Not measured: {unmeasured}.");
    }
    s.push_str(" The tables below the chart give every number.");
    s
}

/// Who each operation is compared with, operations with the same
/// competitor together: `the fastest feature-complete YAML serde library on
/// each workload for serde read and serde write, and yaml-edit for Document
/// parse and Document edit script (insert + set + remove)`.
#[must_use]
pub fn basis(sections: &[Section]) -> String {
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for sec in sections {
        match groups.iter_mut().find(|(v, _)| *v == sec.versus) {
            Some((_, ops)) => ops.push(sec.short.to_owned()),
            None => groups.push((sec.versus, vec![sec.short.to_owned()])),
        }
    }
    let parts: Vec<String> = groups
        .iter()
        .map(|(v, ops)| format!("{v} for {}", and_list(ops)))
        .collect();
    // A serial comma: each part has its own `and` inside.
    match parts.as_slice() {
        [init @ .., last] if !init.is_empty() => format!("{}, and {last}", init.join(", ")),
        _ => parts.concat(),
    }
}

/// How many rows of `sections` satisfy `keep`.
fn count_rows(sections: &[Section], keep: impl Fn(&Row) -> bool) -> usize {
    sections
        .iter()
        .flat_map(|sec| &sec.rows)
        .filter(|r| keep(r))
        .count()
}

/// The rows of `sections` that satisfy `keep`, grouped by operation:
/// `serde read gha-workflow and helm-values; Document parse compose-merge`.
fn rows_matching(sections: &[Section], keep: impl Fn(&Row) -> bool) -> String {
    sections
        .iter()
        .filter_map(|sec| {
            let names: Vec<String> = sec
                .rows
                .iter()
                .filter(|r| keep(r))
                .map(|r| r.workload.clone())
                .collect();
            (!names.is_empty()).then(|| format!("{} {}", sec.short, and_list(&names)))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn html_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The `<picture>`: dark and light sources, and the light image with the
/// generated alt text.
fn picture(alt: &str, light: &str, dark: &str) -> String {
    format!(
        "<picture>\n  \
         <source media=\"(prefers-color-scheme: dark)\" srcset=\"{dark}\">\n  \
         <source media=\"(prefers-color-scheme: light)\" srcset=\"{light}\">\n  \
         <img alt=\"{}\" src=\"{light}\">\n\
         </picture>\n",
        html_attr(alt)
    )
}

/// The READMEs' `<picture>` of the compact chart, at `git_ref` (absolute:
/// the crate README is also shown on crates.io and docs.rs).
fn readme_picture(alt: &str, git_ref: &str) -> String {
    let raw = raw();
    picture(
        alt,
        &format!("{raw}/{git_ref}/{CHART_LIGHT}"),
        &format!("{raw}/{git_ref}/{CHART_DARK}"),
    )
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The wall-clock note under the tables: what the wall-clock column is, and
/// every row where oneyaml is slower (or equal, or not measured) by
/// wall-clock time. Each of the three is its own sentence, so a row named
/// after one clause can't be read as part of another (the rows of one
/// operation are joined with `and`, operations with `;`).
#[must_use]
pub fn wall_strip(sections: &[Section]) -> String {
    let total: usize = sections.iter().map(|sec| sec.rows.len()).sum();
    // As the column shows them: below 1.0 is slower (`0.98× — slower`).
    let slower = |r: &Row| r.wall_faster.is_some_and(|f| f < 1.0);
    let equal = |r: &Row| r.wall == Wall::Tie;
    let missing = |r: &Row| r.wall_faster.is_none();
    let rows = |n: usize| if n == 1 { "row" } else { "rows" };
    let mut s = String::from(
        "**× faster (wall-clock)** = the competitor's median time ÷ oneyaml's on the same \
         host (for serde, from the median throughput); it varies more from run to run than \
         the instruction count.",
    );
    let n = count_rows(sections, slower);
    if n == 0 {
        s.push_str(" oneyaml is not slower in wall-clock time on any row.");
    } else {
        let _ = write!(
            s,
            " oneyaml is slower in wall-clock time on {n} of {total} {}: {}.",
            rows(total),
            rows_matching(sections, slower)
        );
    }
    let n = count_rows(sections, equal);
    if n > 0 {
        let _ = write!(
            s,
            " The medians are equal on {n} {}: {}.",
            rows(n),
            rows_matching(sections, equal)
        );
    }
    let n = count_rows(sections, missing);
    if n > 0 {
        let _ = write!(
            s,
            " Wall-clock time was not measured on {n} {}: {}.",
            rows(n),
            rows_matching(sections, missing)
        );
    }
    s
}

/// `"`rev` on date"`, from the `environment` section, as readers see it.
///
/// When a `Revision note` is recorded the summary shows the commit (the
/// leading `` `…` `` of `Revision`) with the note; otherwise `Revision` as
/// recorded (detected revisions are `` `abc1234` `` or `` `abc1234` +
/// uncommitted changes ``).
///
/// # Errors
/// Missing rows, or a shown revision that names an internal process
/// identifier ([`oracles::public_text`], e.g. a work unit recorded by an
/// older `bench`): re-record the section with a plain `--revision` and a
/// reader-facing `--revision-note`.
fn measured(env: &[(String, String)]) -> Result<String, String> {
    let revision = env_value(env, "Revision")?;
    let shown = match env_value(env, "Revision note") {
        Ok(note) => format!("{} ({note})", commit_of(revision)),
        Err(_) => revision.to_owned(),
    };
    if let Some(found) = oracles::public_text::internal_reference(&shown) {
        return Err(format!(
            "the benchmark summary would show the revision {shown:?}, which contains {found:?}, an \
             internal process identifier readers cannot resolve; re-record the \
             `environment` section with a plain --revision and a reader-facing \
             --revision-note"
        ));
    }
    Ok(format!("{shown} on {}", env_value(env, "Date")?))
}

/// The leading `` `commit` `` of a recorded revision, or all of it.
fn commit_of(revision: &str) -> &str {
    revision
        .strip_prefix('`')
        .and_then(|rest| rest.find('`'))
        .and_then(|end| revision.get(..end.saturating_add(2)))
        .unwrap_or(revision)
}

/// The summary region of `docs/performance.md`: the detailed chart (one
/// labelled bar per row), the tables, and the notes on the ratios, the
/// wall-clock times and the run. Its links are relative and it names no
/// git ref, so it reads the same at every ref.
///
/// # Errors
/// As [`summary`] and [`environment`].
pub fn summary_body(doc: &str) -> Result<String, String> {
    let sections = summary(doc)?;
    let env = environment(doc)?;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "<!-- Generated by `bench readme` from the tables further down this page; do not edit. -->"
    );
    s.push_str(&picture(
        &alt_text(&sections),
        from_docs(DETAIL_LIGHT),
        from_docs(DETAIL_DARK),
    ));
    for sec in &sections {
        let _ = writeln!(
            s,
            "\n| {} | vs | oneyaml {} | competitor {} | × faster | × faster (wall-clock) |\n|---|---|---|---|---|---|",
            sec.title, sec.unit, sec.unit
        );
        for r in &sec.rows {
            let (ours, theirs) = bold_better(&r.ours, &r.theirs);
            let _ = writeln!(
                s,
                "| {} ({}) | {} | {ours} | {theirs} | {} | {} |",
                r.workload,
                size(r.bytes),
                r.competitor,
                faster_cell(r.faster),
                faster_cell(r.wall_faster),
            );
        }
    }
    let _ = writeln!(
        s,
        "\n- **× faster** = the competitor's CPU instructions ÷ oneyaml's on the same input \
         (higher is better; below 1.0× oneyaml is slower), counted by valgrind (callgrind): \
         deterministic from run to run, not wall-clock time. Ir/B = instructions per input \
         byte; the edit script counts one script of three edits (insert + set + remove) on \
         the parsed document; lower is better, the better value in bold."
    );
    let _ = writeln!(s, "- {}", wall_strip(&sections));
    let _ = writeln!(
        s,
        "- **Measured** {} · {} · {} · {} · {}, against {}. Serde rows compare with the \
         fastest feature-complete YAML serde library on that row (aliases, multi-document, \
         merge keys), `Document` rows with yaml-edit, the other lossless editor; every \
         output is checked for equivalent work. Inputs are generated in the shape of GitHub \
         Actions, docker-compose, Helm, OpenAPI and multi-document Kubernetes files.",
        measured(&env)?,
        env_value(&env, "CPU")?,
        env_value(&env, "OS")?,
        env_value(&env, "rustc")?,
        env_value(&env, "valgrind")?,
        competitor_versions(env_value(&env, "Comparators")?, &sections)
    );
    Ok(s)
}

/// The crate README region body: the compact chart and one link to the
/// summary tables, which carry every number, the wall-clock times and the
/// method. The image's alt text carries what the chart shows
/// ([`compact_alt_text`]).
///
/// # Errors
/// As [`summary`] and [`environment`] (the provenance must be readable,
/// though the region does not show it), or a `git_ref` that
/// [`validate_ref`](oracles::readme::validate_ref) rejects.
pub fn readme_body(doc: &str, git_ref: &str) -> Result<String, String> {
    oracles::readme::validate_ref(git_ref)?;
    let sections = summary(doc)?;
    environment(doc)?;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "<!-- Generated by `bench readme` from docs/performance.md; do not edit. -->"
    );
    s.push_str(&readme_picture(&compact_alt_text(&sections), git_ref));
    let _ = writeln!(
        s,
        "\n[Full results, with wall-clock times and the method \u{2192}]\
         ({REPO}/blob/{git_ref}/{PERFORMANCE}#results-at-a-glance)"
    );
    Ok(s)
}

/// The repo-root README region body: the same chart and link as the crate
/// README's region, so the two never disagree.
///
/// # Errors
/// As [`readme_body`].
pub fn chart_body(doc: &str, git_ref: &str) -> Result<String, String> {
    readme_body(doc, git_ref)
}

/// The detailed chart's panels: one row per table row, a row without a
/// ratio kept as a labelled "not measured" row, so the chart, the tables
/// and the alt text list the same rows.
#[must_use]
pub fn panels(sections: &[Section]) -> Vec<Panel> {
    sections
        .iter()
        .map(|sec| Panel {
            title: sec.short.to_owned(),
            bars: sec
                .rows
                .iter()
                .map(|r| Bar {
                    label: format!("{} vs {}", r.workload, r.competitor),
                    value: r.faster,
                    text: ratio_text(r.faster),
                })
                .collect(),
        })
        .collect()
}

/// What one operation's rows add up to, for the compact chart and its alt
/// text: the range and how many rows are slower, faster, equal or not
/// measured. Nothing is averaged: one slower row is a slower count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spread {
    /// `0.55× to 63×`, `1.5×` when every row shows the same, or
    /// `not measured`.
    pub range: String,
    /// The counts, slower first, each with its tone.
    pub counts: Vec<(String, Tone)>,
    /// The slower rows, as `openapi-100k 0.96×`.
    pub slower: Vec<String>,
}

/// The [`Spread`] of one operation.
#[must_use]
pub fn spread(sec: &Section) -> Spread {
    let n = sec.rows.len();
    let measured: Vec<f64> = sec.rows.iter().filter_map(|r| r.faster).collect();
    let range = match (
        measured.iter().copied().reduce(f64::min),
        measured.iter().copied().reduce(f64::max),
    ) {
        (Some(lo), Some(hi)) if sig2(lo) == sig2(hi) => format!("{}×", sig2(lo)),
        (Some(lo), Some(hi)) => format!("{}× to {}×", sig2(lo), sig2(hi)),
        _ => "not measured".to_owned(),
    };
    let count = |keep: &dyn Fn(f64) -> bool| measured.iter().filter(|f| keep(**f)).count();
    let slower_n = count(&|f| f < 1.0);
    let faster_n = count(&|f| f > 1.0);
    let equal_n = measured
        .len()
        .saturating_sub(slower_n)
        .saturating_sub(faster_n);
    let missing_n = n.saturating_sub(measured.len());
    let mut counts = Vec::new();
    if slower_n > 0 {
        counts.push((format!("slower on {slower_n} of {n}"), Tone::Slower));
    }
    if faster_n == n {
        counts.push((format!("faster on all {n}"), Tone::Muted));
    } else if faster_n > 0 {
        counts.push((format!("faster on {faster_n} of {n}"), Tone::Muted));
    }
    if equal_n > 0 {
        counts.push((format!("equal on {equal_n} of {n}"), Tone::Muted));
    }
    if missing_n > 0 {
        counts.push((format!("not measured on {missing_n} of {n}"), Tone::Muted));
    }
    let slower = sec
        .rows
        .iter()
        .filter_map(|r| {
            r.faster
                .filter(|f| *f < 1.0)
                .map(|f| format!("{} {}×", r.workload, sig2(f)))
        })
        .collect();
    Spread {
        range,
        counts,
        slower,
    }
}

/// The competitors of an operation, each once, in row order.
fn competitors(sec: &Section) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in &sec.rows {
        if !out.contains(&r.competitor) {
            out.push(r.competitor.clone());
        }
    }
    out
}

/// The compact chart's groups: one per operation, one bar per row.
#[must_use]
pub fn groups(sections: &[Section]) -> Vec<Group> {
    sections
        .iter()
        .map(|sec| {
            let sp = spread(sec);
            let mut notes = vec![(sp.range, Tone::Text)];
            notes.extend(sp.counts);
            Group {
                title: sec.compact.to_owned(),
                subtitle: format!("vs {}", competitors(sec).join(", ")),
                bars: sec.rows.iter().map(|r| r.faster).collect(),
                notes,
            }
        })
        .collect()
}

/// The workloads the compact chart's bars stand for, top to bottom.
#[must_use]
pub fn bar_workloads(sections: &[Section]) -> Vec<String> {
    sections
        .first()
        .map(|sec| sec.rows.iter().map(|r| r.workload.clone()).collect())
        .unwrap_or_default()
}

/// The READMEs' alt text: what the compact chart measures and, per
/// operation, every number and count it shows, naming each slower row.
#[must_use]
pub fn compact_alt_text(sections: &[Section]) -> String {
    let workloads = bar_workloads(sections);
    let mut s = format!(
        "Chart of CPU instruction counts, not wall-clock time: how many times fewer \
         instructions oneyaml executes than each competitor (competitor ÷ oneyaml; higher is \
         better; 1.0× is equal), against {}; one bar per workload for {} workloads ({}).",
        basis(sections),
        workloads.len(),
        and_list(&workloads)
    );
    for sec in sections {
        let sp = spread(sec);
        let counts: Vec<String> = sp.counts.iter().map(|(c, _)| c.clone()).collect();
        let _ = write!(
            s,
            " {} vs {}: {}; {}",
            sec.short,
            and_list(&competitors(sec)),
            sp.range,
            counts.join(", ")
        );
        if !sp.slower.is_empty() {
            let _ = write!(s, " (slower: {})", and_list(&sp.slower));
        }
        s.push('.');
    }
    s
}

/// The four charts generated from `doc`, each with its path relative to
/// the workspace root: the compact README chart ([`CHART_LIGHT`],
/// [`CHART_DARK`]) and the detailed chart ([`DETAIL_LIGHT`],
/// [`DETAIL_DARK`]). Both share the description that lists every bar.
///
/// # Errors
/// As [`summary`].
pub fn charts(doc: &str) -> Result<[(&'static str, String); 4], String> {
    let sections = summary(doc)?;
    let desc = chart_description(&sections);
    let groups = groups(&sections);
    let workloads = bar_workloads(&sections);
    let panels = panels(&sections);
    let compact = |t: &chart::Theme| chart::compact_svg(&groups, &workloads, t, &desc);
    let detail = |t: &chart::Theme| chart::svg(&panels, t, &desc);
    Ok([
        (CHART_LIGHT, compact(&chart::LIGHT)),
        (CHART_DARK, compact(&chart::DARK)),
        (DETAIL_LIGHT, detail(&chart::LIGHT)),
        (DETAIL_DARK, detail(&chart::DARK)),
    ])
}

/// What [`generate`] wrote.
#[derive(Clone, Debug)]
pub struct Written {
    /// The README.
    pub readme: PathBuf,
    /// The repo-root README.
    pub root_readme: PathBuf,
    /// The file whose summary region was written (the tables' own file).
    pub summary: PathBuf,
    /// The light and dark compact charts.
    pub charts: [PathBuf; 2],
    /// The light and dark detailed charts.
    pub details: [PathBuf; 2],
}

/// Regenerates, from the `docs/performance.md` at `performance`: the
/// README region and the repo-root README's chart region (compact chart
/// and link, pointing at `git_ref`), the summary region of `performance`
/// itself, and the four charts under `root`.
///
/// # Errors
/// As [`readme_body`] and [`summary_body`], plus I/O and marker errors, or
/// generated text that names an internal process identifier
/// ([`oracles::public_text`]); all of it is checked, and every region
/// replaced in memory, before anything is written.
pub fn generate(root: &Path, performance: &Path, git_ref: &str) -> Result<Written, String> {
    let doc = std::fs::read_to_string(performance)
        .map_err(|e| format!("{}: {e}", performance.display()))?;
    let body = readme_body(&doc, git_ref)?;
    let chart_region = chart_body(&doc, git_ref)?;
    let summary_region = summary_body(&doc)?;
    let svgs = charts(&doc)?;
    for (what, text) in [
        (README, body.as_str()),
        (ROOT_README, chart_region.as_str()),
        (PERFORMANCE, summary_region.as_str()),
    ]
    .into_iter()
    .chain(svgs.iter().map(|(path, svg)| (*path, svg.as_str())))
    {
        oracles::public_text::check(what, text)?;
    }
    let readme = root.join(README);
    let root_readme = root.join(ROOT_README);
    let mut edits = Vec::new();
    for (path, section, region) in [
        (&readme, README_SECTION, &body),
        (&root_readme, CHART_SECTION, &chart_region),
    ] {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let new = replace_section(&text, section, region)?;
        edits.push((path.clone(), text, new));
    }
    let new_doc = replace_section(&doc, SUMMARY_SECTION, &summary_region)?;
    edits.push((performance.to_path_buf(), doc, new_doc));
    // Every region is replaced before any file is written.
    for (path, old, new) in &edits {
        if new != old {
            std::fs::write(path, new).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    for (rel, svg) in &svgs {
        let path = root.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, svg).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(Written {
        readme,
        root_readme,
        summary: performance.to_path_buf(),
        charts: [root.join(CHART_LIGHT), root.join(CHART_DARK)],
        details: [root.join(DETAIL_LIGHT), root.join(DETAIL_DARK)],
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn two_significant_figures() {
        assert_eq!(sig2(1.134), "1.1");
        assert_eq!(sig2(0.2824), "0.28");
        assert_eq!(sig2(12.3), "12");
        assert_eq!(sig2(126.0), "130");
        assert_eq!(sig2(9.96), "10");
        assert_eq!(sig2(0.996), "1.0");
        assert_eq!(sig2(3.5713), "3.6");
        assert_eq!(sig2(0.0123), "0.012");
        assert_eq!(sig2(1.0), "1.0");
        assert_eq!(sig2(-1.0), "0");
        assert_eq!(sig2(f64::NAN), "0");
    }

    #[test]
    fn numbers_and_non_numbers() {
        assert_eq!(number("1,058,925"), Some(1_058_925.0));
        assert_eq!(number("**3.57×**"), Some(3.57));
        assert_eq!(number("414.8"), Some(414.8));
        assert_eq!(number("n/a"), None);
        assert_eq!(number("rejects"), None);
        assert_eq!(number("timeout (> 300 s)"), None);
    }

    #[test]
    fn slower_is_marked_never_inverted() {
        assert_eq!(faster_cell(Some(0.28)), "0.28× — slower");
        assert_eq!(faster_cell(Some(0.8)), "0.80× — slower");
        assert_eq!(ratio_text(Some(0.8)), "0.80× — slower");
        assert_eq!(ratio_text(Some(1.0)), "1.0×");
        assert_eq!(ratio_text(None), "not measured");
        assert_eq!(faster_cell(Some(1.13)), "**1.1×**");
        assert_eq!(faster_cell(None), "n/a");
    }

    #[test]
    fn wall_strip_names_every_row_where_oneyaml_is_slower() {
        let row = |w: &str, wall, wall_faster| Row {
            workload: w.to_owned(),
            bytes: 1.0,
            competitor: "c".to_owned(),
            ours: "1".to_owned(),
            theirs: "2".to_owned(),
            faster: Some(2.0),
            wall,
            wall_faster,
        };
        let sec = |short, rows| Section {
            title: "t",
            short,
            compact: short,
            versus: "c",
            unit: "u",
            rows,
        };
        let head = "**× faster (wall-clock)** = the competitor's median time ÷ oneyaml's on the \
                    same host (for serde, from the median throughput); it varies more from run \
                    to run than the instruction count.";
        let all = [sec(
            "read",
            vec![
                row("a", Wall::Agrees, Some(1.2)),
                row("b", Wall::Agrees, Some(3.0)),
            ],
        )];
        assert_eq!(
            wall_strip(&all),
            format!("{head} oneyaml is not slower in wall-clock time on any row.")
        );
        let mixed = [
            sec(
                "read",
                vec![
                    row("a", Wall::Differs, Some(0.8)),
                    row("b", Wall::Agrees, Some(1.1)),
                    // Shown as `1.0× — slower`, so named slower too.
                    row("c", Wall::Differs, Some(0.996)),
                ],
            ),
            sec(
                "parse",
                vec![
                    row("a", Wall::Tie, Some(1.0)),
                    row("b", Wall::Differs, Some(0.5)),
                ],
            ),
            sec("edit", vec![row("e", Wall::Missing, None)]),
        ];
        assert_eq!(
            wall_strip(&mixed),
            format!(
                "{head} oneyaml is slower in wall-clock time on 3 of 6 rows: read a and c; \
                 parse b. The medians are equal on 1 row: parse a. Wall-clock time was not \
                 measured on 1 row: edit e."
            )
        );
        // Several operations in every category: each category is its own
        // sentence, so the last operation named for one can't be read as
        // part of the next, and an equal row never follows the slower
        // ones in the same list.
        let many = [
            sec(
                "serde read",
                vec![
                    row("gha-workflow", Wall::Differs, Some(0.9)),
                    row("compose-merge", Wall::Tie, Some(1.0)),
                    row("helm-values", Wall::Differs, Some(0.7)),
                    row("k8s-multi", Wall::Missing, None),
                ],
            ),
            sec(
                "Document parse",
                vec![
                    row("compose-merge", Wall::Differs, Some(0.95)),
                    row("openapi-100k", Wall::Tie, Some(1.0)),
                    row("k8s-multi", Wall::Missing, None),
                ],
            ),
        ];
        let strip = wall_strip(&many);
        assert_eq!(
            strip,
            format!(
                "{head} oneyaml is slower in wall-clock time on 3 of 7 rows: serde read \
                 gha-workflow and helm-values; Document parse compose-merge. The medians are \
                 equal on 2 rows: serde read compose-merge; Document parse openapi-100k. \
                 Wall-clock time was not measured on 2 rows: serde read k8s-multi; Document \
                 parse k8s-multi."
            )
        );
        // Published text.
        assert!(oracles::public_text::internal_reference(&strip).is_none());
    }

    #[test]
    fn sizes() {
        assert_eq!(size(3078.0), "3.1 kB");
        assert_eq!(size(1_050_323.0), "1.1 MB");
        assert_eq!(size(102_778.0), "100 kB");
        assert_eq!(size(12.0), "12 B");
    }

    #[test]
    fn lock_versions() {
        let lock = "[[package]]\nname = \"yaml-edit\"\nversion = \"0.3.2\"\n";
        assert_eq!(lock_version(lock, "yaml-edit").as_deref(), Some("0.3.2"));
        assert_eq!(lock_version(lock, "yaml"), None);
        let two = "[[package]]\nname = \"oracles\"\nversion = \"0.1.0\"\ndependencies = [\n \"yaml-edit\",\n \"yaml-rust2 0.13.0\",\n]\n\n[[package]]\nname = \"yaml-rust2\"\nversion = \"0.8.1\"\n\n[[package]]\nname = \"yaml-rust2\"\nversion = \"0.13.0\"\n\n[[package]]\nname = \"yaml-edit\"\nversion = \"0.3.2\"\n";
        assert_eq!(
            comparator_version(two, "yaml-rust2").as_deref(),
            Some("0.13.0")
        );
        assert_eq!(
            comparator_version(two, "yaml-edit").as_deref(),
            Some("0.3.2")
        );
        assert_eq!(comparator_version(two, "missing"), None);
    }

    #[test]
    fn environment_is_recorded_with_its_numbers() {
        assert_eq!(with_environment(&["wall"]), ["wall"]);
        assert_eq!(
            with_environment(&["wall", "edits"]),
            [ENVIRONMENT_SECTION, "wall", "edits"]
        );
        for source in README_SOURCES {
            assert_eq!(with_environment(&[source]), [ENVIRONMENT_SECTION, source]);
            assert!(crate::report::SECTIONS.contains(&source));
        }
        let full = with_environment(crate::report::SECTIONS);
        assert_eq!(full, crate::report::SECTIONS, "a full run records it once");
        assert_eq!(full[0], ENVIRONMENT_SECTION);
    }

    #[test]
    fn bold_marks_the_better_value() {
        assert_eq!(bold_better("1", "2"), ("**1**".to_owned(), "2".to_owned()));
        assert_eq!(bold_better("3", "2"), ("3".to_owned(), "**2**".to_owned()));
        assert_eq!(bold_better("2", "2"), ("2".to_owned(), "2".to_owned()));
        assert_eq!(
            bold_better("rejects", "2"),
            ("rejects".to_owned(), "2".to_owned())
        );
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_729), (2026, 10, 3));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(today_utc().len(), 10);
    }

    #[test]
    fn tables_round_trip() {
        let text = "x\n| a | b |\n|---|---|\n| 1 | 2 |\n\n| c |\n|---|\n| 3 |\n";
        let t = tables(text);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].header, ["a", "b"]);
        assert_eq!(t[0].rows, [["1", "2"]]);
        assert_eq!(t[0].row(&[("a", "1")]).unwrap()[1], "2");
        assert!(t[0].row(&[("a", "9")]).is_none());
        assert!(t[0].row(&[("z", "1")]).is_none());
        assert_eq!(t[1].rows, [["3"]]);
    }

    #[test]
    fn missing_markers_are_errors() {
        assert!(section_body("nothing", "x").is_err());
        assert!(section_body("<!-- bench:x:begin -->\n", "x").is_err());
        assert_eq!(
            section_body("<!-- bench:x:begin -->\nB\n<!-- bench:x:end -->", "x").unwrap(),
            "B\n"
        );
    }

    /// Who the fixture's operations are compared with.
    const BASIS: &str = "the fastest feature-complete YAML serde library on each workload for \
                         serde read and serde write, and yaml-edit for Document parse and \
                         Document edit script (insert + set + remove)";

    /// A minimal `docs/performance.md` with every table the summary reads.
    fn fixture(edit_ours: &str) -> String {
        let mut s = String::new();
        s.push_str("<!-- bench:environment:begin -->\n| Measured | |\n|---|---|\n");
        for k in [
            "Revision",
            "Date",
            "CPU",
            "OS",
            "rustc",
            "valgrind",
            "Comparators",
        ] {
            s.push_str(&format!("| {k} | {k}-value |\n"));
        }
        s.push_str("<!-- bench:environment:end -->\n");
        s.push_str("<!-- bench:summary:begin -->\n<!-- bench:summary:end -->\n");
        for (sec, wall) in [
            ("serde-instructions", "serde-wall"),
            ("ser-instructions", "ser-wall"),
        ] {
            s.push_str(&format!("<!-- bench:{sec}:begin -->\n"));
            s.push_str("| Case | Workload | Bytes | oneyaml Ir/B | serde_yaml_ng Ir/B | fastest feature-complete |\n|---|---|---|---|---|---|\n");
            for w in WORKLOADS {
                s.push_str(&format!(
                    "| {w} | typed | 3,078 | 100.0 | 113.0 | serde_yaml_ng |\n"
                ));
                s.push_str(&format!(
                    "| {w} | value | 3,078 | 1.0 | 1.0 | serde_yaml_ng |\n"
                ));
            }
            s.push_str(&format!(
                "<!-- bench:{sec}:end -->\n<!-- bench:{wall}:begin -->\n"
            ));
            s.push_str(
                "| Case | Workload | oneyaml MB/s | serde_yaml_ng MB/s |\n|---|---|---|---|\n",
            );
            for w in WORKLOADS {
                s.push_str(&format!("| {w} | typed | 50 | 40 |\n"));
            }
            s.push_str(&format!("<!-- bench:{wall}:end -->\n"));
        }
        s.push_str("<!-- bench:document:begin -->\n| Case | Bytes | parse oneyaml | parse yaml-edit |\n|---|---|---|---|\n");
        for w in WORKLOADS {
            s.push_str(&format!("| {w} | 1,050,323 | 400.0 | 440.0 |\n"));
        }
        s.push_str("\n| Case | parse ms oneyaml | parse ms yaml-edit |\n|---|---|---|\n");
        for w in WORKLOADS {
            s.push_str(&format!("| {w} | 1.0 | 1.0 |\n"));
        }
        s.push_str("<!-- bench:document:end -->\n<!-- bench:edits:begin -->\n| Case | Bytes | script Ir oneyaml | script Ir yaml-edit |\n|---|---|---|---|\n");
        for w in WORKLOADS {
            s.push_str(&format!("| {w} | 2,442 | {edit_ours} | 302,491 |\n"));
        }
        s.push_str("\n| Case | script ms oneyaml | script ms yaml-edit |\n|---|---|---|\n");
        for w in WORKLOADS {
            s.push_str(&format!("| {w} | 0.035 | 0.015 |\n"));
        }
        s.push_str("<!-- bench:edits:end -->\n");
        s
    }

    #[test]
    fn summary_reads_every_operation() {
        let doc = fixture("1,078,595");
        let secs = summary(&doc).unwrap();
        assert_eq!(secs.len(), 4);
        let read = &secs[0].rows[0];
        assert_eq!(read.competitor, "serde_yaml_ng");
        assert_eq!(read.ours, "100.0");
        assert_eq!(read.theirs, "113.0");
        assert_eq!(sig2(read.faster.unwrap()), "1.1");
        assert_eq!(read.wall, Wall::Agrees);
        assert_eq!(secs[2].rows[0].wall, Wall::Tie);
        // A loss stays a loss: 302,491 ÷ 1,078,595 = 0.28×, and the
        // wall-clock (0.035 ms vs 0.015 ms) also says slower: agrees.
        let edit = &secs[3].rows[0];
        assert_eq!(sig2(edit.faster.unwrap()), "0.28");
        assert_eq!(edit.wall, Wall::Agrees);
        assert_eq!(edit.competitor, "yaml-edit");

        // The summary region: tables, notes and the detailed chart.
        let body = summary_body(&doc).unwrap();
        assert!(body.contains("0.28× — slower"));
        // The better absolute value is bold, whichever side it is on.
        assert!(body.contains(
            "| gha-workflow (2.4 kB) | yaml-edit | 1,078,595 | **302,491** | 0.28× — slower | 0.43× — slower |\n"
        ));
        assert!(body.contains("oneyaml Ir per script"));
        assert!(body.contains("one script of three edits (insert + set + remove)"));
        assert!(body.contains(
            "| gha-workflow (3.1 kB) | serde_yaml_ng | **100.0** | 113.0 | **1.1×** | **1.3×** |\n"
        ));
        assert!(body.contains("| serde read (typed) | vs | oneyaml Ir/B | competitor Ir/B | × faster | × faster (wall-clock) |\n|---|---|---|---|---|---|\n"));
        assert_eq!(tables(&body).len(), 4);
        // Equal wall-clock medians read 1.0×.
        assert!(body.contains("| **1.1×** | **1.0×** |\n"));
        assert!((secs[2].rows[0].wall_faster.unwrap() - 1.0).abs() < 1e-9);
        assert!(body.contains("\n- **× faster** = the competitor's CPU instructions ÷ oneyaml's"));
        assert!(body.contains("- **Measured** Revision-value on Date-value · CPU-value"));
        // Wall-clock: serde rows faster (50 vs 40 MB/s), parse ties, the
        // edit script slower (0.035 ms vs 0.015 ms).
        assert!(body.contains(
            "oneyaml is slower in wall-clock time on 5 of 20 rows: Document edit script \
             (insert + set + remove) gha-workflow, compose-merge, helm-values, openapi-100k \
             and k8s-multi. The medians are equal on 5 rows: Document parse gha-workflow, \
             compose-merge, helm-values, openapi-100k and k8s-multi.\n"
        ));
        // The detailed chart, by relative links: the region names no ref.
        let alt = alt_text(&secs);
        assert!(body.contains(&format!(
            "<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"img/bench-detail-dark.svg\">\n  <source media=\"(prefers-color-scheme: light)\" srcset=\"img/bench-detail-light.svg\">\n  <img alt=\"{}\" src=\"img/bench-detail-light.svg\">\n</picture>\n",
            html_attr(&alt)
        )));
        assert!(!body.contains("https://"), "relative links only");
        assert!(!body.contains("/main/"));
        assert_eq!(summary_body(&doc).unwrap(), body, "byte-stable");
        assert!(!body.contains("```"));

        // The README region: the compact chart and one link, no table.
        let readme = readme_body(&doc, "main").unwrap();
        let raw = raw();
        let compact_alt = compact_alt_text(&secs);
        assert_eq!(
            readme,
            format!(
                "<!-- Generated by `bench readme` from docs/performance.md; do not edit. -->\n\
                 <picture>\n  \
                 <source media=\"(prefers-color-scheme: dark)\" srcset=\"{raw}/main/{CHART_DARK}\">\n  \
                 <source media=\"(prefers-color-scheme: light)\" srcset=\"{raw}/main/{CHART_LIGHT}\">\n  \
                 <img alt=\"{}\" src=\"{raw}/main/{CHART_LIGHT}\">\n\
                 </picture>\n\n\
                 [Full results, with wall-clock times and the method →]\
                 ({REPO}/blob/main/{PERFORMANCE}#results-at-a-glance)\n",
                html_attr(&compact_alt)
            )
        );
        assert!(tables(&readme).is_empty(), "no table in the README region");
        assert!(!readme.contains("**Measured**") && !readme.contains("wall-clock) ="));
        assert_eq!(readme.matches("](").count(), 1, "one link");
        let pinned = readme_body(&doc, "v1.0.0").unwrap();
        assert!(pinned.contains(&format!("srcset=\"{raw}/v1.0.0/{CHART_DARK}\"")));
        assert!(pinned.contains(&format!("srcset=\"{raw}/v1.0.0/{CHART_LIGHT}\"")));
        assert!(pinned.contains(&format!("src=\"{raw}/v1.0.0/{CHART_LIGHT}\"")));
        assert!(pinned.contains(&format!(
            "{REPO}/blob/v1.0.0/{PERFORMANCE}#results-at-a-glance"
        )));
        assert!(!pinned.contains("/main/"));
        assert_eq!(readme_body(&doc, "main").unwrap(), readme, "byte-stable");
        assert_eq!(
            competitor_versions(
                "serde_yaml_ng 0.10.0, serde_json 1.0, yaml-edit 0.3.2",
                &secs
            ),
            "serde_yaml_ng 0.10.0, yaml-edit 0.3.2"
        );
        // The SVGs' description lists every bar.
        assert!(chart_description(&secs).contains(
            "Document edit script (insert + set + remove): gha-workflow 0.28× — slower vs yaml-edit"
        ));
        // The detailed chart's alt text summarises and points at the tables
        // under it.
        assert_eq!(
            alt,
            format!(
                "Bar chart: how many times fewer CPU instructions oneyaml executes than each \
                 competitor (higher is better), against {BASIS}, on 5 workloads. oneyaml \
                 executes more instructions on 5 of 20 rows: Document edit script (insert + set \
                 + remove) gha-workflow, compose-merge, helm-values, openapi-100k and \
                 k8s-multi. The tables below the chart give every number."
            )
        );
        // The README's alt text carries every number the compact chart
        // shows, and names each slower row with its ratio.
        assert_eq!(
            compact_alt,
            format!(
                "Chart of CPU instruction counts, not wall-clock time: how many times fewer \
             instructions oneyaml executes than each competitor (competitor ÷ oneyaml; higher \
             is better; 1.0× is equal), against {BASIS}; one bar per workload for 5 workloads \
             (gha-workflow, compose-merge, helm-values, openapi-100k and k8s-multi). serde read vs serde_yaml_ng: 1.1×; faster on all 5. serde write vs \
             serde_yaml_ng: 1.1×; faster on all 5. Document parse vs yaml-edit: 1.1×; faster \
             on all 5. Document edit script (insert + set + remove) vs yaml-edit: 0.28×; \
             slower on 5 of 5 (slower: gha-workflow 0.28×, compose-merge 0.28×, helm-values \
             0.28×, openapi-100k 0.28× and k8s-multi 0.28×)."
            )
        );
        // Who each operation is compared with: the description and both alt
        // texts say it the same way, and none claims yaml-edit is chosen
        // as the fastest.
        assert_eq!(basis(&secs), BASIS);
        assert!(chart_description(&secs).starts_with(&format!(
            "Bar chart: how many times fewer CPU instructions oneyaml executes than each \
             competitor (competitor ÷ oneyaml; higher is better; 1.0× is equal), against \
             {BASIS}. serde read:"
        )));
        let g = groups(&secs);
        for (group, sec) in g.iter().zip(&secs) {
            let sp = spread(sec);
            assert!(compact_alt.contains(&format!("{}; ", sp.range)));
            for (note, _) in &group.notes {
                assert!(compact_alt.contains(note.as_str()), "{note}");
            }
        }
        assert_eq!(g[3].title, "Document edit script");
        assert_eq!(g[3].subtitle, "vs yaml-edit");
        assert_eq!(g[3].bars.len(), 5);
        assert_eq!(
            g[3].notes,
            [
                ("0.28×".to_owned(), Tone::Text),
                ("slower on 5 of 5".to_owned(), Tone::Slower)
            ]
        );
        let p = panels(&secs);
        assert_eq!(p[3].title, "Document edit script (insert + set + remove)");
        assert_eq!(p[3].bars[0].text, "0.28× — slower");
        assert_eq!(p.iter().map(|x| x.bars.len()).sum::<usize>(), 20);
        // The repo-root region: the same chart and link.
        assert_eq!(chart_body(&doc, "main").unwrap(), readme);
        // A bad ref is refused before anything is written.
        for bad in ["", "a\"b", "<x>", "a b"] {
            assert!(readme_body(&doc, bad).is_err(), "{bad:?}");
            assert!(chart_body(&doc, bad).is_err(), "{bad:?}");
        }
    }

    /// A spread keeps every loss: one slower row is counted and named, with
    /// the range running below 1.0×, whatever the other rows win by.
    #[test]
    fn the_compact_summary_never_hides_a_loss() {
        let row = |w: &str, f: Option<f64>, c: &str| Row {
            workload: w.to_owned(),
            bytes: 1.0,
            competitor: c.to_owned(),
            ours: "1".to_owned(),
            theirs: "1".to_owned(),
            faster: f,
            wall: Wall::Missing,
            wall_faster: None,
        };
        let sec = |rows| Section {
            title: "t",
            short: "edit script",
            compact: "edits",
            versus: "x or y",
            unit: "u",
            rows,
        };
        let mixed = sec(vec![
            row("a", Some(2.2), "x"),
            row("b", Some(0.547), "x"),
            row("c", Some(63.0), "y"),
            row("d", Some(1.0), "x"),
            row("e", None, "y"),
        ]);
        let sp = spread(&mixed);
        assert_eq!(sp.range, "0.55× to 63×");
        assert_eq!(
            sp.counts,
            [
                ("slower on 1 of 5".to_owned(), Tone::Slower),
                ("faster on 2 of 5".to_owned(), Tone::Muted),
                ("equal on 1 of 5".to_owned(), Tone::Muted),
                ("not measured on 1 of 5".to_owned(), Tone::Muted),
            ]
        );
        assert_eq!(sp.slower, ["b 0.55×"]);
        assert_eq!(competitors(&mixed), ["x", "y"]);
        let alt = compact_alt_text(std::slice::from_ref(&mixed));
        assert!(alt.ends_with(
            " edit script vs x and y: 0.55× to 63×; slower on 1 of 5, faster on 2 of 5, equal \
             on 1 of 5, not measured on 1 of 5 (slower: b 0.55×)."
        ));
        let g = &groups(std::slice::from_ref(&mixed))[0];
        assert_eq!(g.subtitle, "vs x, y");
        assert_eq!(
            g.bars,
            [Some(2.2), Some(0.547), Some(63.0), Some(1.0), None]
        );
        // A row shown as `1.0× — slower` counts as slower.
        let near = sec(vec![row("a", Some(0.996), "x"), row("b", Some(1.2), "x")]);
        let sp = spread(&near);
        assert_eq!(sp.range, "1.0× to 1.2×");
        assert_eq!(sp.counts[0], ("slower on 1 of 2".to_owned(), Tone::Slower));
        assert_eq!(sp.slower, ["a 1.0×"]);
        // Nothing measured.
        let none = sec(vec![row("a", None, "x")]);
        let sp = spread(&none);
        assert_eq!(sp.range, "not measured");
        assert_eq!(
            sp.counts,
            [("not measured on 1 of 1".to_owned(), Tone::Muted)]
        );
        assert!(sp.slower.is_empty());
    }

    #[test]
    fn unmeasured_cells_are_reported_not_invented() {
        let doc = fixture("timeout (> 300 s)");
        let secs = summary(&doc).unwrap();
        let edit = &secs[3].rows[0];
        assert!(edit.faster.is_none());
        assert_eq!(edit.wall, Wall::Missing);
        assert!(
            summary_body(&doc)
                .unwrap()
                .contains("| timeout (> 300 s) | 302,491 | n/a | 0.43× — slower |\n")
        );
        // The compact chart and its alt text say so too.
        assert!(compact_alt_text(&secs).ends_with(
            " Document edit script (insert + set + remove) vs yaml-edit: not measured; not \
             measured on 5 of 5."
        ));
        assert!(groups(&secs)[3].bars.iter().all(Option::is_none));
        assert!(chart_description(&secs).contains("gha-workflow not measured vs yaml-edit"));
        assert!(alt_text(&secs).contains(
            " Not measured: Document edit script (insert + set + remove) gha-workflow, \
             compose-merge, helm-values, openapi-100k and k8s-multi."
        ));
        // The chart keeps the row, labelled, with no bar.
        let bars = &panels(&secs)[3].bars;
        assert_eq!(bars.len(), WORKLOADS.len());
        assert!(
            bars.iter()
                .all(|b| b.value.is_none() && b.text == "not measured")
        );
        // A wall-clock cell without a number is n/a, not an error.
        let wall_timeout =
            fixture("1").replace("| 0.035 | 0.015 |", "| timeout (> 75 s) | 0.015 |");
        let secs = summary(&wall_timeout).unwrap();
        assert!(secs[3].rows[0].faster.is_some());
        assert_eq!(secs[3].rows[0].wall, Wall::Missing);
        assert!(wall_strip(&secs).contains(
            " Wall-clock time was not measured on 5 rows: Document edit script (insert + set + \
             remove) gha-workflow, compose-merge, helm-values, openapi-100k and k8s-multi."
        ));
    }

    #[test]
    fn missing_evidence_is_an_error() {
        let doc = fixture("1");
        let no_env = doc.replace("bench:environment:", "bench:gone:");
        assert!(
            readme_body(&no_env, "main")
                .unwrap_err()
                .contains("environment")
        );
        let no_row = doc.replace("| k8s-multi | typed |", "| other | typed |");
        assert!(summary(&no_row).unwrap_err().contains("no row"));
        let no_col = doc.replace("script Ir yaml-edit", "script Ir other");
        assert!(summary(&no_col).is_err());
        assert!(summary("").is_err());
        // Wall-clock evidence: section, table, row, column.
        let no_wall = doc.replace("bench:ser-wall:", "bench:gone:");
        assert!(summary(&no_wall).unwrap_err().contains("ser-wall"));
        let no_wall_table =
            doc.replace("| Case | parse ms oneyaml |", "| Case | parse s oneyaml |");
        assert!(
            summary(&no_wall_table)
                .unwrap_err()
                .contains("parse ms oneyaml")
        );
        let no_wall_row = doc.replace("| helm-values | 0.035 |", "| other | 0.035 |");
        assert!(summary(&no_wall_row).unwrap_err().contains("no row"));
        let no_wall_col = doc.replace("script ms yaml-edit", "script ms other");
        assert!(
            summary(&no_wall_col)
                .unwrap_err()
                .contains("script ms yaml-edit")
        );
        // Bytes: column, and a cell that is not a number.
        let no_bytes = doc.replace(
            "| Case | Bytes | parse oneyaml |",
            "| Case | Size | parse oneyaml |",
        );
        assert!(summary(&no_bytes).unwrap_err().contains("Bytes"));
        let bad_bytes = doc.replace("| gha-workflow | 2,442 |", "| gha-workflow | ? |");
        assert!(summary(&bad_bytes).unwrap_err().contains("not a number"));
    }

    #[test]
    fn environment_section_is_a_parsable_table() {
        let p = Provenance {
            revision: Some("`abc1234`".to_owned()),
            revision_note: None,
            date: Some("2026-10-03".to_owned()),
        };
        let body = environment_section(&p).unwrap();
        let doc =
            format!("<!-- bench:environment:begin -->\n{body}<!-- bench:environment:end -->\n");
        let env = environment(&doc).unwrap();
        assert_eq!(env_value(&env, "Revision").unwrap(), "`abc1234`");
        assert_eq!(env_value(&env, "Date").unwrap(), "2026-10-03");
        // The build the numbers come from.
        assert_eq!(
            env_value(&env, "Build").unwrap(),
            crate::build::description()
        );
        // Every comparator, as `name version` with a dotted numeric
        // version (whatever `Cargo.lock` holds today).
        let recorded = env_value(&env, "Comparators").unwrap();
        let entries: Vec<&str> = recorded.split(", ").collect();
        assert_eq!(entries.len(), COMPARATORS.len());
        for (entry, name) in entries.iter().zip(COMPARATORS) {
            let (n, v) = entry.split_once(' ').unwrap();
            assert_eq!(n, name);
            let parts: Vec<&str> = v.split('.').collect();
            assert_eq!(parts.len(), 3, "{entry}");
            assert!(
                parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
                "{entry}"
            );
        }
        assert!(env_value(&env, "missing").is_err());
        assert!(
            env_value(&env, "Revision note").is_err(),
            "only when recorded"
        );
        assert_eq!(measured(&env).unwrap(), "`abc1234` on 2026-10-03");

        // A recorded note follows the revision; the summary shows the commit
        // and the note.
        let p = Provenance {
            revision: Some("`4c6d70c`".to_owned()),
            revision_note: Some("only the benchmark harness and docs differed".to_owned()),
            date: Some("2026-10-03".to_owned()),
        };
        let body = environment_section(&p).unwrap();
        assert!(body.starts_with(
            "| Measured | |\n|---|---|\n| Revision | `4c6d70c` |\n| Revision note | only the \
             benchmark harness and docs differed |\n| Date | 2026-10-03 |\n"
        ));
        // The section is published: a revision or note naming an internal
        // identifier is refused before anything is recorded.
        for (revision, note) in [
            ("`4c6d70c` + unit 49b (bench crate and docs only)", None),
            ("`4c6d70c`", Some("the Q4.4 audit tree")),
        ] {
            let bad = Provenance {
                revision: Some(revision.to_owned()),
                revision_note: note.map(str::to_owned),
                date: None,
            };
            let err = environment_section(&bad).unwrap_err();
            assert!(err.contains("internal process identifier"), "{err}");
        }
        let doc =
            format!("<!-- bench:environment:begin -->\n{body}<!-- bench:environment:end -->\n");
        let env = environment(&doc).unwrap();
        assert_eq!(
            measured(&env).unwrap(),
            "`4c6d70c` (only the benchmark harness and docs differed) on 2026-10-03"
        );
    }

    #[test]
    fn readers_never_see_work_units() {
        let env = |rev: &str, note: Option<&str>| {
            let mut e = vec![("Revision".to_owned(), rev.to_owned())];
            if let Some(n) = note {
                e.push(("Revision note".to_owned(), n.to_owned()));
            }
            e.push(("Date".to_owned(), "2026-10-03".to_owned()));
            e
        };
        // Without a note, a revision naming a unit is refused, with the fix.
        for rev in ["`4c6d70c` + unit 49b", "`a` + Units 41/41b", "`a` + unit 7"] {
            let err = measured(&env(rev, None)).unwrap_err();
            assert!(err.contains("--revision-note"), "{rev}: {err}");
        }
        // Nor may the note name one.
        assert!(measured(&env("`a`", Some("unit 49b's changes"))).is_err());
        // Detected revisions and words that merely contain "unit" pass.
        for rev in [
            "`abc1234`",
            "`abc1234` + uncommitted changes",
            "`a` + community docs",
            "`a` + unit tests",
            "unknown",
        ] {
            assert!(measured(&env(rev, None)).is_ok(), "{rev}");
        }
        assert_eq!(commit_of("`abc` + x"), "`abc`");
        assert_eq!(commit_of("unknown"), "unknown");
        assert_eq!(commit_of("`open"), "`open");
        assert_eq!(
            measured(&env("unknown", Some("n"))).unwrap(),
            "unknown (n) on 2026-10-03"
        );
    }

    /// The check loop of [`generate`] refuses generated README, chart-region
    /// and SVG text that names an internal process identifier, even where
    /// the recorded revision is plain (so `measured` passes), before any
    /// file is written.
    #[test]
    fn generate_checks_every_output_before_writing() {
        // Under the target directory (`target/<profile>/deps/..`), not /tmp.
        let exe = std::env::current_exe().unwrap();
        let dir = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(format!("bench-readme-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crates/oneyaml")).unwrap();
        std::fs::create_dir_all(dir.join("docs/img")).unwrap();
        let readme = "# x\n\n<!-- bench:readme:begin -->\n<!-- bench:readme:end -->\n";
        let root_text = "# r\n\n<!-- bench:chart:begin -->\n<!-- bench:chart:end -->\n";
        std::fs::write(dir.join(README), readme).unwrap();
        std::fs::write(dir.join(ROOT_README), root_text).unwrap();
        let perf = dir.join(PERFORMANCE);
        let unchanged = |doc: &str| {
            assert_eq!(std::fs::read_to_string(dir.join(README)).unwrap(), readme);
            assert_eq!(
                std::fs::read_to_string(dir.join(ROOT_README)).unwrap(),
                root_text
            );
            assert_eq!(std::fs::read_to_string(&perf).unwrap(), doc);
            for svg in [CHART_LIGHT, CHART_DARK, DETAIL_LIGHT, DETAIL_DARK] {
                assert!(!dir.join(svg).exists(), "{svg}");
            }
        };
        // Only in the summary region (the Measured line's CPU cell).
        std::fs::write(
            &perf,
            fixture("1").replace("| CPU | CPU-value |", "| CPU | the NN-3 host |"),
        )
        .unwrap();
        let doc = std::fs::read_to_string(&perf).unwrap();
        assert!(
            measured(&environment(&doc).unwrap()).is_ok(),
            "not `measured`"
        );
        let err = generate(&dir, &perf, DEFAULT_REF).unwrap_err();
        assert!(err.starts_with(PERFORMANCE), "{err}");
        assert!(err.contains("\"NN-3\""), "{err}");
        unchanged(&doc);
        // In every output: a competitor name reaches both alt texts, both
        // regions, the summary and every SVG; the first checked output is
        // named.
        let doc = fixture("1").replace("serde_yaml_ng", "OI-2-yaml");
        std::fs::write(&perf, &doc).unwrap();
        let err = generate(&dir, &perf, DEFAULT_REF).unwrap_err();
        assert!(err.starts_with(README), "{err}");
        assert!(err.contains("\"OI-2\""), "{err}");
        unchanged(&doc);
        // The same fixture, plain, is written.
        std::fs::write(&perf, fixture("1")).unwrap();
        generate(&dir, &perf, DEFAULT_REF).unwrap();
        for svg in [CHART_LIGHT, CHART_DARK, DETAIL_LIGHT, DETAIL_DARK] {
            assert!(dir.join(svg).exists(), "{svg}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn generate_writes_readme_and_charts_byte_stably() {
        // Under the target directory (`target/<profile>/deps/..`), not /tmp.
        let exe = std::env::current_exe().unwrap();
        let dir = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(format!("bench-readme-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crates/oneyaml")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        let perf = dir.join(PERFORMANCE);
        std::fs::write(&perf, fixture("1,078,595")).unwrap();
        std::fs::write(
            dir.join(README),
            "# x\n\n<!-- bench:readme:begin -->\n<!-- bench:readme:end -->\n\n## Quick start\n",
        )
        .unwrap();
        let root_text = "# r\n\n<!-- bench:chart:begin -->\n<!-- bench:chart:end -->\n\nprose\n";
        std::fs::write(dir.join(ROOT_README), root_text).unwrap();
        let w = generate(&dir, &perf, DEFAULT_REF).unwrap();
        assert_eq!(w.summary, perf);
        let files = |w: &Written| -> Vec<Vec<u8>> {
            [
                &w.readme,
                &w.root_readme,
                &w.charts[0],
                &w.charts[1],
                &w.summary,
                &w.details[0],
                &w.details[1],
            ]
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect()
        };
        let first = files(&w);
        let w2 = generate(&dir, &perf, DEFAULT_REF).unwrap();
        assert_eq!(first, files(&w2), "a second run changes nothing");
        let readme = String::from_utf8(first[0].clone()).unwrap();
        assert!(readme.starts_with("# x\n\n<!-- bench:readme:begin -->\n<!-- Generated"));
        assert!(readme.ends_with("<!-- bench:readme:end -->\n\n## Quick start\n"));
        let root = String::from_utf8(first[1].clone()).unwrap();
        assert!(root.starts_with("# r\n\n<!-- bench:chart:begin -->\n<!-- Generated"));
        assert!(root.ends_with("<!-- bench:chart:end -->\n\nprose\n"));
        // The READMEs carry the chart and its link, no table; the tables
        // and notes are in the summary region, around the hand-written text.
        let doc = fixture("1,078,595");
        for text in [&readme, &root] {
            assert!(tables(text).is_empty(), "{text}");
            assert!(!text.contains("**× faster (wall-clock)**"));
            assert!(text.contains(&readme_body(&doc, DEFAULT_REF).unwrap()));
        }
        let written = String::from_utf8(first[4].clone()).unwrap();
        assert_eq!(
            written,
            replace_section(&doc, SUMMARY_SECTION, &summary_body(&doc).unwrap()).unwrap()
        );
        let region = section_body(&written, SUMMARY_SECTION).unwrap();
        assert_eq!(tables(region).len(), 4);
        assert!(region.contains("**× faster (wall-clock)**"));
        assert!(region.contains("- **Measured** "));
        // The rest of the file is untouched, and the summary still reads
        // the same rows from it.
        assert_eq!(written.replace(region, ""), doc);
        assert_eq!(summary_body(&written).unwrap(), summary_body(&doc).unwrap());
        // The SVGs keep a title and the full per-bar description; each
        // image's alt text is shorter.
        let secs = summary(&doc).unwrap();
        for svg in first[2..4].iter().chain(&first[5..]) {
            let svg = String::from_utf8(svg.clone()).unwrap();
            assert!(svg.contains("<title id=\"title\">"));
            assert!(svg.contains("aria-labelledby=\"title desc\""));
            assert!(svg.contains(&format!(
                "<desc id=\"desc\">{}</desc>",
                chart_description(&secs)
            )));
        }
        let compact = String::from_utf8(first[2].clone()).unwrap();
        assert!(compact.contains(">Document edit script</text>"));
        assert!(compact.contains(">slower on 5 of 5</text>"));
        let detail = String::from_utf8(first[5].clone()).unwrap();
        assert!(detail.contains(">Document edit script (insert + set + remove)</text>"));
        assert!(readme.contains(&html_attr(&compact_alt_text(&secs))));
        assert!(region.contains(&html_attr(&alt_text(&secs))));
        assert!(!readme.contains(&html_attr(&chart_description(&secs))));
        // A missing root region fails before either README is touched.
        std::fs::write(dir.join(ROOT_README), "no markers\n").unwrap();
        std::fs::write(
            dir.join(README),
            "# x\n\n<!-- bench:readme:begin -->\n<!-- bench:readme:end -->\n",
        )
        .unwrap();
        assert!(
            generate(&dir, &perf, DEFAULT_REF)
                .unwrap_err()
                .contains("bench:chart")
        );
        assert_eq!(
            std::fs::read_to_string(&w.readme).unwrap(),
            "# x\n\n<!-- bench:readme:begin -->\n<!-- bench:readme:end -->\n"
        );
        assert!(generate(&dir, &perf, "bad ref").is_err());
        // Generated text naming an internal identifier (here a recorded
        // revision from before the check existed) is refused, nothing written.
        let before: Vec<Vec<u8>> = [&w.readme, &w.charts[0], &w.details[0]]
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        let stale = dir.join("docs/stale.md");
        std::fs::write(
            &stale,
            fixture("1").replace("| Revision-value |", "| `abc` + unit 49b |"),
        )
        .unwrap();
        assert!(
            generate(&dir, &stale, DEFAULT_REF)
                .unwrap_err()
                .contains("internal process identifier")
        );
        let after: Vec<Vec<u8>> = [&w.readme, &w.charts[0], &w.details[0]]
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert_eq!(before, after);
        std::fs::write(dir.join(ROOT_README), root_text).unwrap();
        // Tables without a summary region: refused before either README
        // is written.
        let readmes = || [&w.readme, &w.root_readme].map(|p| std::fs::read_to_string(p).unwrap());
        let before = readmes();
        let no_summary = dir.join("docs/no-summary.md");
        std::fs::write(
            &no_summary,
            fixture("1").replace(
                "<!-- bench:summary:begin -->\n<!-- bench:summary:end -->\n",
                "",
            ),
        )
        .unwrap();
        assert!(
            generate(&dir, &no_summary, DEFAULT_REF)
                .unwrap_err()
                .contains("bench:summary")
        );
        assert_eq!(readmes(), before);
        assert!(generate(&dir, &dir.join("missing.md"), DEFAULT_REF).is_err());
        std::fs::write(dir.join(README), "no markers\n").unwrap();
        assert!(generate(&dir, &perf, DEFAULT_REF).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
