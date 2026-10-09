//! Release link pinning (`bench pin-links <ref>`).
//!
//! The published surfaces (the READMEs, `CHANGELOG.md`, `docs/**/*.md` and
//! the rustdoc comments of the three published crates) link to files in
//! the repository by git ref: `<REPO>/blob/<ref>/…`, `<REPO>/tree/<ref>/…`
//! and `https://raw.githubusercontent.com/<owner>/<name>/<ref>/…`, where
//! `<REPO>` is the workspace `repository` field
//! ([`oracles::readme::REPO`], the one place the URL is written).
//! Day to day every such link names `main`. A published crate's docs are
//! frozen at release, so they must not follow a moving branch: on the
//! release commit, `bench pin-links v1.2.3` repoints every link to the tag;
//! afterwards, `bench pin-links main` on `main` repoints them back.
//!
//! The rewrite is textual and covers the generated regions too (the
//! `bench:readme`/`bench:chart` regions and the `oracles:conformance`
//! line write their links the same way), so a release is one step. Before
//! anything is written it checks that the rewritten `bench:readme` and
//! `bench:chart` regions are exactly what `bench readme --ref <ref>`
//! generates, i.e. the textual rewrite equals regeneration, and that the
//! `bench:summary` region of `docs/performance.md` (relative links only,
//! the same at every ref) is current.
//!
//! It is a bijection on the state it accepts: every link must name the
//! same ref before the rewrite (a mix is refused, with each ref's first
//! location), so `main → tag → main` restores the exact bytes. Running it
//! twice with the same ref changes nothing the second time. Writing is
//! all or nothing ([`apply`]).
//!
//! # Link forms
//!
//! Every link to the repository is classified, never skipped silently:
//!
//! - `/blob/<ref>/…`, `/tree/<ref>/…` and raw links are repointed;
//! - links that name no ref (the repository itself, `/issues/…`,
//!   `/pulls`, `/discussions`, `/security`, `/actions`, `/wiki`,
//!   `/releases` and `/releases/latest`, …: [`REFLESS`]) are left alone;
//! - every other form (`/raw/`, `/edit/`, `/blame/`, `/commits/`,
//!   `/commit/`, `/compare/`, `/archive/`, `/releases/tag/`, or a form this
//!   module does not know) is refused, with its location: it may name a
//!   ref the rewrite would leave on `main`.
//!
//! A link that must keep its ref (evidence at a commit SHA, a link to an
//! older release) is exempted by putting [`FIXED_MARKER`]
//! (`<!-- pin-links: fixed -->`, invisible in rendered markdown and
//! rustdoc) on its line: every repository link on that line is left as it
//! is, whatever its form, and the report counts them.
//!
//! Refs are checked by [`validate_ref`](oracles::readme::validate_ref),
//! which refuses `/`: a ref is read back out of a URL as one path segment.
//! `bench readme --ref` and `oracles --readme --ref` use the same check.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::markdown::replace_section;
use crate::readme;

/// Marks a line whose repository links keep their ref (see the module
/// docs).
pub const FIXED_MARKER: &str = "<!-- pin-links: fixed -->";

/// The first path segments after the repository URL of the link forms
/// that name no git ref. `releases` counts only when it is not followed
/// by `/tag/` or `/download/`.
pub const REFLESS: [&str; 20] = [
    "issues",
    "pulls",
    "pull",
    "discussions",
    "security",
    "actions",
    "wiki",
    "labels",
    "milestones",
    "projects",
    "stargazers",
    "watchers",
    "forks",
    "fork",
    "network",
    "pulse",
    "graphs",
    "tags",
    "branches",
    "releases",
];

/// The published surfaces' fixed markdown files, relative to the root.
pub const MARKDOWN: [&str; 5] = [
    "README.md",
    "CHANGELOG.md",
    "crates/oneyaml/README.md",
    "crates/oneyaml-core/README.md",
    "crates/oneyaml-syntax/README.md",
];

/// Directories whose `*.md` files (recursively) are published surfaces.
pub const MARKDOWN_DIRS: [&str; 1] = ["docs"];

/// Directories whose `*.rs` files' doc comments (`///`, `//!`) are
/// published surfaces (rustdoc on docs.rs).
pub const RUSTDOC_DIRS: [&str; 3] = [
    "crates/oneyaml/src",
    "crates/oneyaml-core/src",
    "crates/oneyaml-syntax/src",
];

/// One file of the published surfaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Relative to the workspace root, `/`-separated.
    pub path: String,
    /// Whether only doc-comment lines are surfaces (`.rs`).
    pub rustdoc: bool,
}

/// Every published surface under `root`, in a stable order. Missing
/// optional files (`MARKDOWN` entries) are skipped.
///
/// Symlinks are never followed: a symlinked directory is not walked (no
/// cycles; its target, when it is a surface, is walked on its own), and a
/// symlinked surface file is refused (rewriting it would write through the
/// link, or replace it).
///
/// # Errors
/// I/O errors while listing the directories; a symlinked surface file.
pub fn surfaces(root: &Path) -> Result<Vec<Surface>, String> {
    let mut out = Vec::new();
    for p in MARKDOWN {
        let full = root.join(p);
        match std::fs::symlink_metadata(&full) {
            Ok(m) if m.file_type().is_symlink() => return Err(symlinked(p)),
            Ok(m) if m.is_file() => out.push(Surface {
                path: p.to_owned(),
                rustdoc: false,
            }),
            _ => {}
        }
    }
    for (dirs, ext, rustdoc) in [
        (&MARKDOWN_DIRS[..], "md", false),
        (&RUSTDOC_DIRS[..], "rs", true),
    ] {
        for dir in dirs {
            let mut found = Vec::new();
            walk(root, &root.join(dir), ext, &mut found)?;
            found.sort();
            out.extend(found.into_iter().map(|path| Surface { path, rustdoc }));
        }
    }
    Ok(out)
}

fn symlinked(path: &str) -> String {
    format!(
        "{path} is a symlink; pin-links rewrites regular files only (link to the file it \
         points at, or replace the symlink with the file)"
    )
}

/// `p` relative to `root`, `/`-separated.
fn relative(root: &Path, p: &Path) -> String {
    let rel = p.strip_prefix(root).unwrap_or(p);
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

/// Files with extension `ext` under `dir` (explicit stack), as
/// root-relative `/`-separated paths. Symlinks are not followed
/// ([`surfaces`]).
fn walk(root: &Path, dir: &Path, ext: &str, out: &mut Vec<String>) -> Result<(), String> {
    // A symlinked start directory is not walked either.
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", d.display()))?;
            // `DirEntry::file_type` does not follow symlinks.
            let kind = entry
                .file_type()
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
            let p = entry.path();
            let wanted = p.extension().is_some_and(|e| e == ext);
            if kind.is_symlink() {
                if wanted {
                    return Err(symlinked(&relative(root, &p)));
                }
            } else if kind.is_dir() {
                stack.push(p);
            } else if wanted {
                out.push(relative(root, &p));
            }
        }
    }
    Ok(())
}

/// A ref named by a repointable link: its byte range in the text and its
/// line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Byte range of the ref segment.
    pub range: std::ops::Range<usize>,
    /// 1-based line number.
    pub line: usize,
}

/// A repository link whose form may name a ref the rewrite does not
/// repoint ([`scan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// 1-based line number.
    pub line: usize,
    /// The form, e.g. `/blame/`.
    pub form: String,
}

/// The repository links of one text, classified (module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// The repointable links, in text order.
    pub links: Vec<Link>,
    /// Links of a form that is refused.
    pub refused: Vec<Refused>,
    /// Links on lines marked [`FIXED_MARKER`], left as they are.
    pub fixed: usize,
}

fn is_ref_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// The leading run of ref characters of `s`.
fn ref_run(s: &str) -> &str {
    let run = s.find(|c: char| !is_ref_char(c)).unwrap_or(s.len());
    s.get(..run).unwrap_or_default()
}

/// The repository URLs a link starts with: the GitHub URL and the raw
/// base, both from the workspace `repository` field.
struct Bases {
    github: &'static str,
    raw: String,
}

impl Bases {
    fn new() -> Self {
        Self {
            github: oracles::readme::REPO,
            raw: oracles::readme::raw_base(),
        }
    }
}

/// What a link at `rest` (the text right after the repository URL) is.
enum Form<'a> {
    /// Not a link to this repository (e.g. `<REPO>-other`).
    Other,
    /// Names no ref.
    Refless,
    /// Names a ref the rewrite repoints, starting `skip` bytes into `rest`.
    Pinned { skip: usize, seg: &'a str },
    /// Refused, with its form.
    Refused(String),
}

/// Classifies a GitHub link by the text after `<REPO>`.
fn github_form(rest: &str) -> Form<'_> {
    let Some(path) = rest.strip_prefix('/') else {
        // `<REPO>` itself (followed by `)`, `>`, a space, `.`, `.git`, `#`…),
        // or a longer name that is another repository.
        return match rest.chars().next() {
            Some(c) if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') => Form::Other,
            _ => Form::Refless,
        };
    };
    let seg = ref_run(path);
    let after = path.get(seg.len()..).unwrap_or_default();
    match seg {
        "" => Form::Refless,
        "blob" | "tree" => match after.strip_prefix('/') {
            Some(r) => Form::Pinned {
                skip: 1usize.saturating_add(seg.len()).saturating_add(1),
                seg: ref_segment(r),
            },
            None => Form::Refused(format!("/{seg}")),
        },
        "releases" => {
            let next = after.strip_prefix('/').map(ref_run).unwrap_or_default();
            if matches!(next, "tag" | "download") {
                Form::Refused(format!("/releases/{next}/"))
            } else {
                Form::Refless
            }
        }
        s if REFLESS.contains(&s) => Form::Refless,
        s => Form::Refused(format!("/{s}/")),
    }
}

/// The ref segment at the start of `s`: a run of ref characters, without a
/// trailing `.` (a ref never ends with `.`, `validate_ref`; the `.` ends the
/// sentence).
fn ref_segment(s: &str) -> &str {
    ref_run(s).trim_end_matches('.')
}

/// The repository links of `text`, classified; for `rustdoc` sources,
/// only in `///` and `//!` lines.
#[must_use]
pub fn scan(text: &str, rustdoc: bool) -> Scan {
    let bases = Bases::new();
    let mut out = Scan::default();
    let mut offset = 0usize;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let line_no = n.saturating_add(1);
        let doc_line = {
            let t = line.trim_start();
            t.starts_with("///") || t.starts_with("//!")
        };
        if !rustdoc || doc_line {
            let fixed = line.contains(FIXED_MARKER);
            for (i, _) in line.match_indices(bases.github) {
                let start = i.saturating_add(bases.github.len());
                let rest = line.get(start..).unwrap_or_default();
                match github_form(rest) {
                    Form::Other => {}
                    _ if fixed => out.fixed = out.fixed.saturating_add(1),
                    Form::Refless => {}
                    Form::Pinned { skip, seg } => {
                        let a = offset.saturating_add(start).saturating_add(skip);
                        out.links.push(Link {
                            range: a..a.saturating_add(seg.len()),
                            line: line_no,
                        });
                    }
                    Form::Refused(form) => out.refused.push(Refused {
                        line: line_no,
                        form,
                    }),
                }
            }
            for (i, _) in line.match_indices(bases.raw.as_str()) {
                let start = i.saturating_add(bases.raw.len());
                let rest = line.get(start..).unwrap_or_default();
                if rest.starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
                    continue; // another repository
                }
                if fixed {
                    out.fixed = out.fixed.saturating_add(1);
                    continue;
                }
                match rest.strip_prefix('/') {
                    Some(r) if !ref_segment(r).is_empty() => {
                        let a = offset.saturating_add(start).saturating_add(1);
                        out.links.push(Link {
                            range: a..a.saturating_add(ref_segment(r).len()),
                            line: line_no,
                        });
                    }
                    _ => out.refused.push(Refused {
                        line: line_no,
                        form: "raw.githubusercontent.com without a ref".to_owned(),
                    }),
                }
            }
        }
        offset = offset.saturating_add(line.len());
    }
    out.links.sort_by_key(|l| l.range.start);
    out
}

/// The ref segments of every repointable link in `text` ([`scan`]).
#[must_use]
pub fn links(text: &str, rustdoc: bool) -> Vec<Link> {
    scan(text, rustdoc).links
}

/// `text` with every link's ref replaced by `to`.
fn rewrite(text: &str, found: &[Link], to: &str) -> String {
    let mut s = String::with_capacity(text.len());
    let mut at = 0usize;
    for l in found {
        s.push_str(text.get(at..l.range.start).unwrap_or_default());
        s.push_str(to);
        at = l.range.end;
    }
    s.push_str(text.get(at..).unwrap_or_default());
    s
}

/// One file the plan changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Relative to the workspace root.
    pub path: String,
    /// The links repointed in it.
    pub links: usize,
    /// The contents the plan was made from ([`apply`] checks them).
    pub old: String,
    /// The new contents.
    pub text: String,
}

/// What [`plan`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The ref every link named before (`None`: there were no links).
    pub from: Option<String>,
    /// The ref every link names after.
    pub to: String,
    /// The links found, in all surfaces.
    pub total: usize,
    /// The links on lines marked [`FIXED_MARKER`], left as they are.
    pub fixed: usize,
    /// The files that change (empty when every link already names `to`).
    pub changes: Vec<Change>,
}

impl Plan {
    /// A human-readable report of what [`apply`] changed (`applied`) or
    /// would change.
    #[must_use]
    pub fn report(&self, applied: bool) -> String {
        let mut s = String::new();
        match &self.from {
            Some(from) if !self.changes.is_empty() => {
                let _ = writeln!(
                    s,
                    "{} {} in {} from `{from}` to `{}`:",
                    if applied {
                        "repointed"
                    } else {
                        "would repoint"
                    },
                    count(self.total, "link"),
                    count(self.changes.len(), "file"),
                    self.to
                );
                for c in &self.changes {
                    let _ = writeln!(s, "  {} ({})", c.path, count(c.links, "link"));
                }
            }
            _ => {
                let _ = writeln!(
                    s,
                    "{} already point at `{}`; nothing to change",
                    count(self.total, "link"),
                    self.to
                );
            }
        }
        if self.fixed > 0 {
            let _ = writeln!(
                s,
                "left {} marked `{FIXED_MARKER}` as {}",
                count(self.fixed, "link"),
                if self.fixed == 1 { "it is" } else { "they are" }
            );
        }
        s
    }
}

/// `"1 link"`, `"2 links"`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Plans repointing every link of the published surfaces under `root` to
/// `to`, reading `docs/performance.md` to check the generated regions.
/// Nothing is written.
///
/// # Errors
/// An invalid `to`; a link whose ref is not a valid ref; a link of a
/// refused form (module docs); links naming different refs; generated
/// regions that differ from what `bench readme --ref <from>` writes; I/O
/// errors; a symlinked surface ([`surfaces`]).
pub fn plan(root: &Path, to: &str) -> Result<Plan, String> {
    oracles::readme::validate_ref(to)?;
    let mut files = Vec::new();
    // ref -> first location
    let mut refs: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;
    let mut fixed = 0usize;
    for s in surfaces(root)? {
        let full = root.join(&s.path);
        let text =
            std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))?;
        let found = scan(&text, s.rustdoc);
        if let Some(r) = found.refused.first() {
            return Err(format!(
                "{}:{}: a repository link of the form `{}` may name a git ref, and pin-links \
                 only repoints `/blob/<ref>/`, `/tree/<ref>/` and raw links; link through one \
                 of those, or, if the link must keep its ref (e.g. a commit SHA), put \
                 `{FIXED_MARKER}` on its line",
                s.path, r.line, r.form
            ));
        }
        for l in &found.links {
            let r = text.get(l.range.clone()).unwrap_or_default();
            oracles::readme::validate_ref(r)
                .map_err(|e| format!("{}:{}: link with {e}", s.path, l.line))?;
            if !refs.iter().any(|(x, _)| x == r) {
                refs.push((r.to_owned(), format!("{}:{}", s.path, l.line)));
            }
        }
        total = total.saturating_add(found.links.len());
        fixed = fixed.saturating_add(found.fixed);
        files.push((s.path, text, found.links));
    }
    let from = match refs.as_slice() {
        [] => None,
        [(r, _)] => Some(r.clone()),
        many => {
            let list: Vec<String> = many.iter().map(|(r, at)| format!("`{r}` ({at})")).collect();
            return Err(format!(
                "the published links name {} different refs: {}; they must all name one ref \
                 (so the rewrite can be undone byte for byte); a link that must keep its ref \
                 (e.g. a commit SHA) is exempted by `{FIXED_MARKER}` on its line",
                many.len(),
                list.join(", ")
            ));
        }
    };
    let mut changes = Vec::new();
    if from.as_deref().is_some_and(|f| f != to) {
        for (path, text, found) in files {
            if !found.is_empty() {
                let new = rewrite(&text, &found, to);
                changes.push(Change {
                    path,
                    links: found.len(),
                    old: text,
                    text: new,
                });
            }
        }
    }
    check_generated(root, from.as_deref().unwrap_or(to), to, &changes)?;
    Ok(Plan {
        from,
        to: to.to_owned(),
        total,
        fixed,
        changes,
    })
}

/// The text of `path` after `changes`.
fn after(root: &Path, path: &str, changes: &[Change]) -> Result<String, String> {
    match changes.iter().find(|c| c.path == path) {
        Some(c) => Ok(c.text.clone()),
        None => {
            let full = root.join(path);
            std::fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))
        }
    }
}

/// The rewritten generated regions equal what `bench readme --ref <to>`
/// writes: the textual rewrite is a regeneration. `from` is the ref the
/// tree's links name now; a stale region is fixed by regenerating at it.
fn check_generated(root: &Path, from: &str, to: &str, changes: &[Change]) -> Result<(), String> {
    let doc = after(root, readme::PERFORMANCE, changes)?;
    for (path, section, body) in [
        (
            readme::PERFORMANCE,
            readme::SUMMARY_SECTION,
            readme::summary_body(&doc)?,
        ),
        (
            readme::README,
            readme::README_SECTION,
            readme::readme_body(&doc, to)?,
        ),
        (
            readme::ROOT_README,
            readme::CHART_SECTION,
            readme::chart_body(&doc, to)?,
        ),
    ] {
        let text = after(root, path, changes)?;
        if replace_section(&text, section, &body)? != text {
            return Err(format!(
                "the `bench:{section}` region of {path} is not what `bench readme` generates \
                 from {}; regenerate it at the ref the published links name now with \
                 `cargo run -p bench --release -- readme --ref {from}`, then pin",
                readme::PERFORMANCE
            ));
        }
    }
    Ok(())
}

/// The file operations [`apply`] uses (a seam for its failure tests).
trait Io {
    fn write(&mut self, path: &Path, text: &str) -> std::io::Result<()>;
    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()>;
}

struct Fs;

impl Io for Fs {
    fn write(&mut self, path: &Path, text: &str) -> std::io::Result<()> {
        std::fs::write(path, text)
    }
    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }
}

/// The staging file of `target`: a hidden sibling, so the rename that
/// commits it stays on one file system.
fn staging(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.pin-links.tmp"))
}

/// Writes the plan's changes under `root`, all or nothing; returns the
/// paths written.
///
/// Every file is first checked to still hold what the plan read, then
/// the new contents are staged beside it (`.<name>.pin-links.tmp`), then
/// each staged file is renamed over its target. A failure while checking
/// or staging writes nothing (the staged files are removed); a failure
/// while renaming restores the files already renamed to their old
/// contents. Either way the tree is left as it was, so the run can be
/// repeated; the error says what happened, and names any file it could
/// not restore.
///
/// # Errors
/// A file changed since the plan was made; I/O errors (with the outcome).
pub fn apply(root: &Path, plan: &Plan) -> Result<Vec<PathBuf>, String> {
    apply_with(root, plan, &mut Fs)
}

fn apply_with(root: &Path, plan: &Plan, io: &mut dyn Io) -> Result<Vec<PathBuf>, String> {
    let targets: Vec<PathBuf> = plan.changes.iter().map(|c| root.join(&c.path)).collect();
    // 1. Unchanged since the plan.
    for (c, p) in plan.changes.iter().zip(&targets) {
        let now = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        if now != c.old {
            return Err(format!(
                "{} changed since the links were planned; nothing was written (run pin-links \
                 again)",
                c.path
            ));
        }
    }
    // 2. Stage.
    let mut staged: Vec<PathBuf> = Vec::new();
    for (c, p) in plan.changes.iter().zip(&targets) {
        let tmp = staging(p);
        let result = io.write(&tmp, &c.text).and_then(|()| {
            // Keep the target's permissions.
            let perm = std::fs::metadata(p)?.permissions();
            std::fs::set_permissions(&tmp, perm)
        });
        if let Err(e) = result {
            staged.push(tmp);
            let left = remove_all(&staged);
            return Err(format!(
                "staging {}: {e}; nothing was written{left}",
                c.path
            ));
        }
        staged.push(tmp);
    }
    // 3. Commit.
    for (i, ((c, p), tmp)) in plan.changes.iter().zip(&targets).zip(&staged).enumerate() {
        if let Err(e) = io.rename(tmp, p) {
            let left = remove_all(staged.get(i..).unwrap_or_default());
            let done = plan.changes.get(..i).unwrap_or_default();
            let mut lost = Vec::new();
            for (d, dp) in done.iter().zip(&targets) {
                if let Err(e) = std::fs::write(dp, &d.old) {
                    lost.push(format!("{} ({e})", d.path));
                }
            }
            return Err(if lost.is_empty() {
                format!(
                    "replacing {}: {e}; restored the {} already repointed, nothing \
                     changed{left}",
                    c.path,
                    count(i, "file")
                )
            } else {
                format!(
                    "replacing {}: {e}; could not restore {} of the {} already repointed: {}; \
                     restore them from version control (`git checkout -- <file>`) before \
                     running pin-links again{left}",
                    c.path,
                    lost.len(),
                    count(i, "file"),
                    lost.join(", ")
                )
            });
        }
    }
    Ok(targets)
}

/// Removes `paths`, best effort; describes the ones left behind.
fn remove_all(paths: &[PathBuf]) -> String {
    let left: Vec<String> = paths
        .iter()
        .filter(|p| p.exists() && std::fs::remove_file(p).is_err())
        .map(|p| p.display().to_string())
        .collect();
    if left.is_empty() {
        String::new()
    } else {
        format!(" (could not remove the staging files {})", left.join(", "))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use oracles::readme::REPO;

    fn refs(text: &str, rustdoc: bool) -> Vec<&str> {
        links(text, rustdoc)
            .into_iter()
            .map(|l| &text[l.range])
            .collect()
    }

    fn raw() -> String {
        oracles::readme::raw_base()
    }

    #[test]
    fn finds_every_link_form() {
        let raw = raw();
        let t = format!(
            "[a]({REPO}/blob/main/docs/x.md#y) <{REPO}/tree/main>\n\
             src=\"{raw}/v1.0.0/docs/img/a.svg\" see {REPO}/tree/main.\n\
             {REPO}/issues/new {REPO}\n"
        );
        assert_eq!(refs(&t, false), ["main", "main", "v1.0.0", "main"]);
        let l = links(&t, false);
        assert_eq!(l.iter().map(|l| l.line).collect::<Vec<_>>(), [1, 1, 2, 2]);
    }

    /// Every repository link is classified; forms that may name a
    /// ref the rewrite does not repoint are refused, not skipped.
    #[test]
    fn link_forms_are_classified() {
        let raw = raw();
        for refless in [
            format!("<{REPO}>"),
            format!("[x]({REPO})"),
            format!("{REPO}."),
            format!("{REPO}.git"),
            format!("{REPO}/"),
            format!("{REPO}/issues/new/choose"),
            format!("{REPO}/pulls"),
            format!("{REPO}/pull/12/files"),
            format!("{REPO}/discussions"),
            format!("{REPO}/security/advisories/new"),
            format!("{REPO}/releases"),
            format!("{REPO}/releases/latest"),
            format!("{REPO}-other/blob/x/a.md"),
            format!("{raw}-other/x/a.md"),
        ] {
            let s = scan(&refless, false);
            assert_eq!(s, Scan::default(), "{refless}");
        }
        for (refused, form) in [
            ("/raw/main/a.md", "/raw/"),
            ("/edit/main/a.md", "/edit/"),
            ("/blame/main/a.md", "/blame/"),
            ("/commits/main", "/commits/"),
            ("/commit/0123abc", "/commit/"),
            ("/compare/v1...main", "/compare/"),
            ("/archive/refs/tags/v1.zip", "/archive/"),
            ("/releases/tag/v1.0.0", "/releases/tag/"),
            ("/releases/download/v1.0.0/x", "/releases/download/"),
            ("/tree", "/tree"),
            ("/something-new/main", "/something-new/"),
        ] {
            let t = format!("a\n[x]({REPO}{refused})\n");
            let s = scan(&t, false);
            assert!(s.links.is_empty(), "{refused}");
            assert_eq!(
                s.refused,
                [Refused {
                    line: 2,
                    form: form.to_owned()
                }],
                "{refused}"
            );
        }
        let s = scan(&format!("{raw}\n{raw}/\n"), false);
        assert_eq!(s.refused.len(), 2);
    }

    /// A marked line keeps every repository link on it as it is.
    #[test]
    fn marked_lines_are_left_alone() {
        let t = format!(
            "[evidence]({REPO}/blob/0123abcd/a.md) and [blame]({REPO}/blame/main/b.md) \
             {FIXED_MARKER}\n[x]({REPO}/blob/main/c.md)\n"
        );
        let s = scan(&t, false);
        assert_eq!(s.fixed, 2);
        assert!(s.refused.is_empty());
        assert_eq!(refs(&t, false), ["main"]);
        // In rustdoc, too.
        let r = format!("/// [e]({REPO}/commit/0123abc) {FIXED_MARKER}\n");
        assert_eq!(scan(&r, true).fixed, 1);
    }

    #[test]
    fn rust_sources_only_in_doc_comments() {
        let t = format!(
            "//! [x]({REPO}/blob/main/a.md)\n\
             const A: &str = \"{REPO}/blob/main/b.md\";\n    \
             /// [y]({REPO}/blob/main/c.md)\n\
             // {REPO}/blob/main/d.md\n"
        );
        let l = links(&t, true);
        assert_eq!(l.iter().map(|l| l.line).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(links(&t, false).len(), 4);
        let out = rewrite(&t, &l, "v2.0.0");
        assert!(out.contains("blob/v2.0.0/a.md") && out.contains("blob/v2.0.0/c.md"));
        assert!(out.contains("blob/main/b.md") && out.contains("blob/main/d.md"));
    }

    #[test]
    fn refs_are_checked() {
        let root = workspace();
        for bad in ["", "a b", "release/1.0", "-x", "v1.", "<x>"] {
            assert!(plan(&root, bad).is_err(), "{bad:?}");
        }
        // The same check as `bench readme --ref` and `oracles --readme --ref`.
        let e = plan(&root, "release/1.0").unwrap_err();
        assert_eq!(Err(e), oracles::readme::validate_ref("release/1.0"));
    }

    /// A scratch tree under the target directory (never /tmp).
    fn scratch(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let dir = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(format!("bench-pin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn workspace() -> PathBuf {
        readme::workspace_root()
    }

    /// A copy of every surface (and the files the region check reads)
    /// into `dir`.
    fn copy_surfaces(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let root = workspace();
        let mut paths: Vec<String> = surfaces(&root)
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        paths.push(readme::PERFORMANCE.to_owned());
        paths.sort();
        paths.dedup();
        let mut copied = Vec::new();
        for p in paths {
            let bytes = std::fs::read(root.join(&p)).unwrap();
            let to = dir.join(&p);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::write(&to, &bytes).unwrap();
            copied.push((p, bytes));
        }
        copied
    }

    fn snapshot(dir: &Path, files: &[(String, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
        files
            .iter()
            .map(|(p, _)| (p.clone(), std::fs::read(dir.join(p)).unwrap()))
            .collect()
    }

    /// No staging file is left anywhere under `dir`.
    fn no_staging_files(dir: &Path) {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let name = p.file_name().unwrap().to_string_lossy().into_owned();
                    assert!(!name.ends_with(".pin-links.tmp"), "{}", p.display());
                }
            }
        }
    }

    /// The ref the copied tree's links name now: `main` day to day, the
    /// tag on a release commit (after `pin-links <tag>`). The tests below
    /// hold whichever it is, so the tagged commit passes the gate too.
    fn current(dir: &Path) -> String {
        plan(dir, "main")
            .unwrap()
            .from
            .expect("the published docs link by ref")
    }

    /// `want`, or another valid tag when the tree already names `want`.
    fn other_than(cur: &str, want: &'static str) -> &'static str {
        if cur == want { "v0.0.0-other" } else { want }
    }

    /// The release round trip on a copy of this workspace's published
    /// surfaces: current ref → tag rewrites every link (generated regions
    /// included), is idempotent, and tag → current ref restores every byte.
    #[test]
    fn tag_and_back_restores_the_exact_bytes() {
        let dir = scratch("round-trip");
        let original = copy_surfaces(&dir);

        let cur = current(&dir);
        let tag = other_than(&cur, "v9.8.7");
        let p = plan(&dir, &cur).unwrap();
        assert_eq!(p.from.as_deref(), Some(cur.as_str()));
        assert!(p.changes.is_empty() && p.total > 40, "{}", p.report(true));
        assert!(p.report(true).contains("nothing to change"));

        let p = plan(&dir, tag).unwrap();
        assert!(p.report(true).starts_with(&format!(
            "repointed {} links in {} files from `{cur}` to `{tag}`:\n",
            p.total,
            p.changes.len()
        )));
        assert!(p.report(false).starts_with("would repoint "));
        assert_eq!(count(1, "link"), "1 link");
        apply(&dir, &p).unwrap();
        no_staging_files(&dir);
        // Every published link, generated regions included, names the tag.
        for s in surfaces(&dir).unwrap() {
            let text = std::fs::read_to_string(dir.join(&s.path)).unwrap();
            for l in links(&text, s.rustdoc) {
                assert_eq!(&text[l.range], tag, "{}:{}", s.path, l.line);
            }
        }
        let readme_text = std::fs::read_to_string(dir.join(readme::README)).unwrap();
        assert!(readme_text.contains(&format!("{}/{tag}/docs/img/bench-summary-dark.svg", raw())));
        assert!(readme_text.contains(&format!("/blob/{tag}/docs/conformance.md")));
        let pinned = snapshot(&dir, &original);
        assert_ne!(pinned, original);

        // Idempotent.
        let again = plan(&dir, tag).unwrap();
        assert!(again.changes.is_empty());
        apply(&dir, &again).unwrap();
        assert_eq!(snapshot(&dir, &original), pinned);

        // And back, byte for byte.
        let back = plan(&dir, &cur).unwrap();
        assert_eq!(back.from.as_deref(), Some(tag));
        apply(&dir, &back).unwrap();
        assert_eq!(snapshot(&dir, &original), original);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mixed_refs_and_stale_regions_are_refused_before_writing() {
        let dir = scratch("refused");
        let original = copy_surfaces(&dir);
        let cur = current(&dir);
        let old = other_than(&cur, "v0.1.0");
        let pin = other_than(&cur, "v1.0.0");
        let tag = other_than(&cur, "v9.8.7");

        // A hand-written link to another ref.
        let docs = dir.join("docs/comparison.md");
        let text = std::fs::read_to_string(&docs).unwrap();
        let mixed = text.replacen(&format!("/blob/{cur}/"), &format!("/blob/{old}/"), 1);
        assert_ne!(mixed, text);
        std::fs::write(&docs, &mixed).unwrap();
        let err = plan(&dir, pin).unwrap_err();
        assert!(
            err.contains("2 different refs")
                && err.contains(&format!("`{old}` (docs/comparison.md:"))
                && err.contains(FIXED_MARKER),
            "{err}"
        );
        // Marked as fixed, it no longer blocks the pin, and stays.
        let first = mixed.find(&format!("/blob/{old}/")).unwrap();
        let eol = first + mixed[first..].find('\n').unwrap();
        let marked = format!("{} {FIXED_MARKER}{}", &mixed[..eol], &mixed[eol..]);
        std::fs::write(&docs, &marked).unwrap();
        let p = plan(&dir, pin).unwrap();
        assert_eq!(p.fixed, 1, "{}", p.report(false));
        assert!(
            p.report(false)
                .contains(&format!("left 1 link marked `{FIXED_MARKER}` as it is")),
            "{}",
            p.report(false)
        );
        let c = p
            .changes
            .iter()
            .find(|c| c.path == "docs/comparison.md")
            .unwrap();
        assert!(
            c.text.contains(&format!("/blob/{old}/")) && !c.text.contains(&format!("/blob/{cur}/"))
        );
        std::fs::write(&docs, &text).unwrap();

        // A link whose ref segment is not a ref.
        std::fs::write(&docs, format!("{text}\n[x]({REPO}/blob/-x/a.md)\n")).unwrap();
        assert!(plan(&dir, pin).unwrap_err().contains("docs/comparison.md:"));
        // A link of a refused form, with its location and the way out.
        let lines = text.lines().count();
        std::fs::write(&docs, format!("{text}[x]({REPO}/blame/main/a.md)\n")).unwrap();
        let err = plan(&dir, pin).unwrap_err();
        assert!(
            err.starts_with(&format!("docs/comparison.md:{}: ", lines + 1))
                && err.contains("`/blame/`")
                && err.contains(FIXED_MARKER),
            "{err}"
        );
        std::fs::write(&docs, &text).unwrap();

        // A generated region that is not what `bench readme` writes: the
        // advice regenerates at the ref the tree names now.
        let readme_path = dir.join(readme::README);
        let r = std::fs::read_to_string(&readme_path).unwrap();
        let stale = r.replacen("from docs/performance.md; do not edit.", "do not edit.", 1);
        assert_ne!(stale, r);
        std::fs::write(&readme_path, &stale).unwrap();
        let err = plan(&dir, pin).unwrap_err();
        assert!(
            err.contains("bench:readme") && err.contains(&format!("readme --ref {cur}`, then pin")),
            "{err}"
        );
        std::fs::write(&readme_path, &r).unwrap();

        // A stale cell in the summary region of docs/performance.md: the
        // region names no ref, so the rewrite would leave it as it is, and
        // it is refused rather than published stale.
        let perf_path = dir.join(readme::PERFORMANCE);
        let perf = std::fs::read_to_string(&perf_path).unwrap();
        let region = perf.find("<!-- bench:summary:begin -->").unwrap();
        let cell = region + perf[region..].find("| **").unwrap();
        let stale = format!("{}| **9{}", &perf[..cell], &perf[cell + 4..]);
        assert!(readme::section_body(&stale, readme::SUMMARY_SECTION).is_ok());
        std::fs::write(&perf_path, &stale).unwrap();
        let err = plan(&dir, pin).unwrap_err();
        assert!(
            err.contains("`bench:summary` region of docs/performance.md")
                && err.contains(&format!("readme --ref {cur}`, then pin")),
            "{err}"
        );
        // Nothing written: every other file as it was, the stale one as left.
        let now = snapshot(&dir, &original);
        for ((path, before), (_, after)) in original.iter().zip(&now) {
            if path == readme::PERFORMANCE {
                assert_eq!(after, stale.as_bytes());
            } else {
                assert_eq!(after, before, "{path}");
            }
        }
        std::fs::write(&perf_path, &perf).unwrap();

        assert_eq!(snapshot(&dir, &original), original);

        // Pinned to another ref, the advice names that ref, not the one
        // asked for.
        apply(&dir, &plan(&dir, tag).unwrap()).unwrap();
        let pinned = std::fs::read_to_string(&readme_path).unwrap();
        std::fs::write(
            &readme_path,
            pinned.replacen("from docs/performance.md; do not edit.", "do not edit.", 1),
        )
        .unwrap();
        for to in [cur.as_str(), tag] {
            let err = plan(&dir, to).unwrap_err();
            assert!(
                err.contains(&format!("readme --ref {tag}`, then pin")),
                "{err}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Fails the `n`th call of `step` (`"write"` or `"rename"`).
    struct Failing {
        step: &'static str,
        n: usize,
        calls: usize,
    }

    impl Io for Failing {
        fn write(&mut self, path: &Path, text: &str) -> std::io::Result<()> {
            self.check("write")?;
            Fs.write(path, text)
        }
        fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.check("rename")?;
            Fs.rename(from, to)
        }
    }

    impl Failing {
        fn check(&mut self, step: &str) -> std::io::Result<()> {
            if step == self.step {
                self.calls = self.calls.saturating_add(1);
                if self.calls == self.n {
                    return Err(std::io::Error::other("injected"));
                }
            }
            Ok(())
        }
    }

    /// An I/O error part-way leaves the tree as it was (never
    /// half-pinned), says so, and the run can simply be repeated.
    #[test]
    fn apply_is_all_or_nothing() {
        let dir = scratch("atomic");
        let original = copy_surfaces(&dir);
        let tag = other_than(&current(&dir), "v9.8.7");
        let p = plan(&dir, tag).unwrap();
        assert!(p.changes.len() >= 3);

        let err = apply_with(
            &dir,
            &p,
            &mut Failing {
                step: "write",
                n: 2,
                calls: 0,
            },
        )
        .unwrap_err();
        assert!(
            err.starts_with(&format!(
                "staging {}: injected; nothing was written",
                p.changes[1].path
            )),
            "{err}"
        );
        assert_eq!(snapshot(&dir, &original), original);
        no_staging_files(&dir);

        let err = apply_with(
            &dir,
            &p,
            &mut Failing {
                step: "rename",
                n: 3,
                calls: 0,
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            format!(
                "replacing {}: injected; restored the 2 files already repointed, nothing changed",
                p.changes[2].path
            )
        );
        assert_eq!(snapshot(&dir, &original), original);
        no_staging_files(&dir);

        // A file edited after planning is not overwritten.
        let first = dir.join(&p.changes[0].path);
        std::fs::write(&first, "edited\n").unwrap();
        let err = apply(&dir, &p).unwrap_err();
        assert!(err.contains("changed since the links were planned; nothing was written"));
        std::fs::write(
            &first,
            &original
                .iter()
                .find(|(f, _)| *f == p.changes[0].path)
                .unwrap()
                .1,
        )
        .unwrap();

        // The same plan then applies.
        apply(&dir, &p).unwrap();
        assert!(plan(&dir, tag).unwrap().changes.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Symlinks are not followed (no cycles), and a symlinked
    /// surface file is refused rather than written through.
    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let dir = scratch("symlinks");
        std::fs::create_dir_all(dir.join("docs/sub")).unwrap();
        std::fs::write(dir.join("docs/a.md"), "a\n").unwrap();
        std::fs::write(dir.join("docs/sub/b.md"), "b\n").unwrap();
        std::os::unix::fs::symlink(dir.join("docs"), dir.join("docs/sub/loop")).unwrap();
        let found: Vec<String> = surfaces(&dir)
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(found, ["docs/a.md", "docs/sub/b.md"]);

        std::os::unix::fs::symlink(dir.join("docs/a.md"), dir.join("docs/c.md")).unwrap();
        let err = surfaces(&dir).unwrap_err();
        assert!(err.starts_with("docs/c.md is a symlink"), "{err}");
        std::fs::remove_file(dir.join("docs/c.md")).unwrap();

        std::os::unix::fs::symlink(dir.join("docs/a.md"), dir.join("README.md")).unwrap();
        assert!(
            surfaces(&dir)
                .unwrap_err()
                .starts_with("README.md is a symlink")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
