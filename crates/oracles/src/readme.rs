//! The generated conformance line of `crates/oneyaml/README.md`.
//!
//! The README states oneyaml's yaml-test-suite pass counts at its claim
//! site (the documentation standard's §4: `n/total` next to the claim,
//! never hand-typed). They are written between
//! `<!-- oracles:conformance:begin/end -->` by
//! `cargo run -p oracles --release -- --readme [--ref GIT-REF]`, from the
//! same [`score`](test_suite::standard::score) run whose numbers
//! `docs/conformance.md` shows and `tests/scores.rs` pins, plus the strict
//! harness run (`test-suite/tests/parser.rs` pins it). `tests/scores.rs`
//! also checks that the README holds exactly the line this module writes
//! from a fresh run, so the README cannot disagree with the evidence.
//!
//! [`validate_ref`] is shared with the `bench` crate's `--ref`.

use std::path::{Path, PathBuf};

use test_suite::runner::Report;
use test_suite::standard::StandardReport;

/// The marker name of the region.
pub const REGION: &str = "oracles:conformance";

/// Where the repository lives; the README is also the docs.rs crate root,
/// so the link is absolute.
///
/// This is the workspace `repository` field of `Cargo.toml`, the one place
/// the URL is written: every generated link (`bench readme`,
/// `--readme`) and every link `bench pin-links` recognises is derived from
/// it, so a repository rename is a one-line change that cannot apply
/// partially. It must be `https://github.com/<owner>/<name>` (checked at
/// compile time, see [`REPO_PATH`]).
pub const REPO: &str = env!("CARGO_PKG_REPOSITORY");

/// The prefix [`REPO`] must start with.
pub const GITHUB: &str = "https://github.com/";

/// The host of raw file URLs (`<RAW_HOST><owner>/<name>/<ref>/<path>`).
pub const RAW_HOST: &str = "https://raw.githubusercontent.com/";

/// `<owner>/<name>` of [`REPO`].
pub const REPO_PATH: &str = repo_path(REPO);

// A `repository` field that is not a GitHub repository URL stops
// the build rather than generating (or failing to recognise) wrong links.
const _: () = assert!(
    is_owner_and_name(REPO_PATH),
    "the workspace `repository` must be https://github.com/<owner>/<name>"
);

/// `url` without [`GITHUB`], or `""` when it does not start with it.
const fn repo_path(url: &str) -> &str {
    let Some((head, tail)) = url.as_bytes().split_at_checked(GITHUB.len()) else {
        return "";
    };
    let mut a = head;
    let mut b = GITHUB.as_bytes();
    while let ([x, ra @ ..], [y, rb @ ..]) = (a, b) {
        if *x != *y {
            return "";
        }
        a = ra;
        b = rb;
    }
    match std::str::from_utf8(tail) {
        Ok(s) => s,
        Err(_) => "",
    }
}

/// `owner/name`: two non-empty segments of ASCII letters, digits, `.`, `_`
/// and `-`.
const fn is_owner_and_name(p: &str) -> bool {
    let mut rest = p.as_bytes();
    let mut slashes = 0u8;
    let mut segment_len = 0usize;
    while let [c, tail @ ..] = rest {
        if *c == b'/' {
            if segment_len == 0 || slashes > 0 {
                return false;
            }
            slashes = 1;
            segment_len = 0;
        } else if c.is_ascii_alphanumeric() || matches!(*c, b'.' | b'_' | b'-') {
            segment_len = 1;
        } else {
            return false;
        }
        rest = tail;
    }
    slashes == 1 && segment_len > 0
}

/// The raw-file base of [`REPO`]: `https://raw.githubusercontent.com/<owner>/<name>`
/// (a raw link is `<base>/<ref>/<path>`).
#[must_use]
pub fn raw_base() -> String {
    format!("{RAW_HOST}{REPO_PATH}")
}

/// The git ref the link points at unless told otherwise.
pub const DEFAULT_REF: &str = "main";

/// The README, relative to the workspace root.
pub const README: &str = "crates/oneyaml/README.md";

/// The evidence the line links to, relative to the workspace root.
pub const CONFORMANCE: &str = "docs/conformance.md";

/// The workspace root this crate was built from.
#[must_use]
pub fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

/// Checks a git ref given for generated links and image URLs (`--ref`).
///
/// Accepts a branch or tag name such as `main` or `v1.0.0`: ASCII letters,
/// digits and `.`, `_`, `-`, at most 100 bytes, not starting with `-` or
/// `.`, not ending with `.`, without `..`, and not ending in `.lock` (a
/// subset of `git check-ref-format`). Everything it accepts is safe
/// unescaped inside an HTML attribute, a markdown link and a URL path.
///
/// `/` is refused: `bench pin-links` reads a ref back out of a
/// link as one path segment, so a ref with `/` (`release/1.0`) could be
/// written by `--ref` but never repointed. This one check serves `bench
/// readme --ref`, `oracles --readme --ref` and `bench pin-links`, so they
/// agree.
///
/// # Errors
/// What is wrong with `r`.
pub fn validate_ref(r: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("invalid --ref {r:?}: {why}"));
    if r.is_empty() {
        return bad("empty");
    }
    if r.len() > 100 {
        return bad("longer than 100 bytes");
    }
    if r.contains('/') {
        return bad(
            "`/` is not allowed (a ref is read back out of a link as one path segment); \
             use a tag such as `v1.0.0`",
        );
    }
    if let Some(c) = r
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return bad(&format!(
            "{c:?} is not allowed (only ASCII letters, digits, `.`, `_`, `-`)"
        ));
    }
    if r.starts_with(['-', '.']) {
        return bad("starts with `-` or `.`");
    }
    if r.ends_with('.') || r.ends_with(".lock") {
        return bad("ends with `.` or `.lock`");
    }
    if r.contains("..") {
        return bad("contains `..`");
    }
    Ok(())
}

/// The numbers the line states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conformance {
    /// The yaml-test-suite data release, as `SOURCE.md` records it.
    pub release: String,
    /// Standard criterion, valid cases: (passed, total).
    pub valid: (usize, usize),
    /// Standard criterion, error cases: (passed, total).
    pub error: (usize, usize),
    /// Strict harness: (passed, total).
    pub strict: (usize, usize),
    /// Valid cases whose event source slices give back the input exactly.
    pub round_trips: usize,
}

/// The data release recorded in the vendored suite's `SOURCE.md`
/// (its `**Release tag:** \`…\`` line).
///
/// # Errors
/// If the file or the line is missing.
pub fn release(data_dir: &Path) -> Result<String, String> {
    let path = data_dir.join("SOURCE.md");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .find_map(|l| l.strip_prefix("- **Release tag:** `"))
        .and_then(|rest| rest.split('`').next())
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: no `- **Release tag:** `…`` line", path.display()))
}

/// The numbers from `ours` (the `oneyaml-syntax` standard report) and
/// `strict` (the strict harness run).
///
/// # Errors
/// If `ours` is not the `oneyaml-syntax` report, has no event profile, or
/// the two runs scored different numbers of cases.
pub fn conformance(
    release: String,
    ours: &StandardReport,
    strict: &Report,
) -> Result<Conformance, String> {
    if ours.name != "oneyaml-syntax" || ours.profile.is_none() {
        return Err(format!(
            "the conformance line needs the oneyaml-syntax report, got `{}`",
            ours.name
        ));
    }
    let c = Conformance {
        release,
        valid: ours.standard(false),
        error: ours.standard(true),
        strict: (strict.passed(), strict.total()),
        round_trips: ours.round_trips(),
    };
    if c.valid.1.saturating_add(c.error.1) != c.strict.1 {
        return Err(format!(
            "the standard run scored {} cases, the strict run {}",
            c.valid.1.saturating_add(c.error.1),
            c.strict.1
        ));
    }
    Ok(c)
}

/// The region's text (one line).
///
/// # Errors
/// As [`validate_ref`].
pub fn line(c: &Conformance, git_ref: &str) -> Result<String, String> {
    validate_ref(git_ref)?;
    let (vp, vt) = c.valid;
    let (ep, et) = c.error;
    let total = vt.saturating_add(et);
    Ok(format!(
        "yaml-test-suite `{}`, {total} cases: **{}/{total}** under the standard criterion \
         (valid {vp}/{vt}, error {ep}/{et}), **{}/{}** strict (exact events, error \
         prefixes and codes), and {}/{vt} valid inputs round-trip byte for byte \
         ([evidence, other Rust parsers' scores and how to reproduce]({REPO}/blob/{git_ref}/{CONFORMANCE})).",
        c.release,
        vp.saturating_add(ep),
        c.strict.0,
        c.strict.1,
        c.round_trips,
    ))
}

fn markers() -> (String, String) {
    (
        format!("<!-- {REGION}:begin -->"),
        format!("<!-- {REGION}:end -->"),
    )
}

/// The region body currently in `doc` (between the markers, without the
/// markers' indentation and line breaks).
///
/// # Errors
/// Missing or out-of-order markers.
pub fn current(doc: &str) -> Result<&str, String> {
    let (begin, end) = markers();
    let b = doc
        .find(&begin)
        .map(|b| b.saturating_add(begin.len()))
        .ok_or_else(|| format!("missing marker {begin:?}"))?;
    let rest = doc.get(b..).unwrap_or_default();
    let e = rest
        .find(&end)
        .ok_or_else(|| format!("missing marker {end:?} after {begin:?}"))?;
    Ok(rest.get(..e).unwrap_or_default().trim())
}

/// `doc` with the region's body replaced by `body` (one line), indented
/// like the begin marker so the region can sit inside a list item.
///
/// # Errors
/// Missing or out-of-order markers, or a begin marker that is not alone
/// on its line.
pub fn replace(doc: &str, body: &str) -> Result<String, String> {
    let (begin, end) = markers();
    let b = doc
        .find(&begin)
        .ok_or_else(|| format!("missing marker {begin:?}"))?;
    let line_start = doc
        .get(..b)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |i| i.saturating_add(1));
    let indent = doc.get(line_start..b).unwrap_or_default();
    if !indent.chars().all(|c| c == ' ') {
        return Err(format!("{begin:?} must start its line"));
    }
    let after_begin = b.saturating_add(begin.len());
    if doc.get(after_begin..).is_none_or(|r| !r.starts_with('\n')) {
        return Err(format!("{begin:?} must end its line"));
    }
    let body_start = after_begin.saturating_add(1);
    let e = doc
        .get(body_start..)
        .and_then(|rest| rest.find(&end))
        .map(|off| body_start.saturating_add(off))
        .ok_or_else(|| format!("missing marker {end:?} after {begin:?}"))?;
    match (doc.get(..body_start), doc.get(e..)) {
        (Some(head), Some(tail)) => Ok(format!("{head}{indent}{body}\n{indent}{tail}")),
        _ => Err("marker offsets out of range".to_owned()),
    }
}

/// Rewrites the region of the README under `root`; returns its path.
///
/// # Errors
/// I/O and marker errors, or a `body` naming an internal process
/// identifier ([`crate::public_text`]).
pub fn write(root: &Path, body: &str) -> Result<PathBuf, String> {
    crate::public_text::check("the README's conformance line", body)?;
    let path = root.join(README);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let new = replace(&text, body)?;
    if new != text {
        std::fs::write(&path, new).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn refs() {
        for ok in ["main", "v1.0.0", "a_b-c.d", "abc1234", "v1.0.0-rc.1"] {
            assert_eq!(validate_ref(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            " main",
            "main ",
            "a\"b",
            "a<b",
            "a>b",
            "a`b",
            "a'b",
            "a b",
            "a&b",
            "a?b",
            "a#b",
            "a%b",
            "-x",
            ".x",
            "/x",
            "x/",
            "x.",
            "a..b",
            "a//b",
            "x.lock",
            "é",
            "release/1.0",
        ] {
            assert!(validate_ref(bad).is_err(), "{bad:?}");
        }
        assert!(
            validate_ref("release/1.0")
                .unwrap_err()
                .contains("`/` is not allowed")
        );
    }

    /// The repository URL comes from the workspace `repository`
    /// field, and the derived forms agree with it.
    #[test]
    fn repository_is_the_workspace_field() {
        let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
        assert!(
            manifest.contains(&format!("repository = \"{REPO}\"\n")),
            "{REPO}"
        );
        assert_eq!(format!("{GITHUB}{REPO_PATH}"), REPO);
        assert_eq!(
            raw_base(),
            format!("https://raw.githubusercontent.com/{REPO_PATH}")
        );
        assert_eq!(repo_path("https://github.com/a/b"), "a/b");
        assert_eq!(repo_path("https://gitlab.com/a/b"), "");
        assert_eq!(repo_path("https://github.co"), "");
        assert!(is_owner_and_name("a-b/c.d_e"));
        for bad in ["", "a", "a/", "/b", "a/b/c", "a b/c", "a/b?"] {
            assert!(!is_owner_and_name(bad), "{bad:?}");
        }
        assert!(validate_ref(&"a".repeat(101)).is_err());
        assert!(validate_ref(&"a".repeat(100)).is_ok());
    }

    fn sample() -> Conformance {
        Conformance {
            release: "data-x".to_owned(),
            valid: (3, 3),
            error: (1, 2),
            strict: (4, 5),
            round_trips: 3,
        }
    }

    #[test]
    fn the_line() {
        let l = line(&sample(), "main").unwrap();
        assert_eq!(
            l,
            format!(
                "yaml-test-suite `data-x`, 5 cases: **4/5** under the standard criterion (valid 3/3, \
             error 1/2), **4/5** strict (exact events, error prefixes and codes), and 3/3 \
             valid inputs round-trip byte for byte ([evidence, other Rust parsers' scores and how to reproduce]\
             ({REPO}/blob/main/docs/conformance.md))."
            )
        );
        assert!(
            line(&sample(), "v1.0.0")
                .unwrap()
                .contains("/blob/v1.0.0/docs/")
        );
        assert!(line(&sample(), "a\"b").is_err());
        assert!(!l.contains('\n'));
    }

    #[test]
    fn replaces_inside_a_list_item_byte_stably() {
        let doc = "- **lead.**\n  <!-- oracles:conformance:begin -->\n  old\n  more\n  \
                   <!-- oracles:conformance:end -->\n- next\n";
        let new = replace(doc, "NEW").unwrap();
        assert_eq!(
            new,
            "- **lead.**\n  <!-- oracles:conformance:begin -->\n  NEW\n  \
             <!-- oracles:conformance:end -->\n- next\n"
        );
        assert_eq!(replace(&new, "NEW").unwrap(), new);
        assert_eq!(current(&new).unwrap(), "NEW");
        let flush = "<!-- oracles:conformance:begin -->\n<!-- oracles:conformance:end -->\n";
        assert_eq!(
            replace(flush, "X").unwrap(),
            "<!-- oracles:conformance:begin -->\nX\n<!-- oracles:conformance:end -->\n"
        );
    }

    #[test]
    fn marker_errors() {
        assert!(replace("nothing", "x").is_err());
        assert!(current("nothing").is_err());
        assert!(replace("<!-- oracles:conformance:begin -->\n", "x").is_err());
        let reversed = "<!-- oracles:conformance:end -->\n<!-- oracles:conformance:begin -->\n";
        assert!(replace(reversed, "x").is_err());
        assert!(current(reversed).is_err());
        let inline = "text <!-- oracles:conformance:begin -->\n<!-- oracles:conformance:end -->";
        assert!(replace(inline, "x").unwrap_err().contains("start its line"));
        let same_line = "<!-- oracles:conformance:begin --> x\n<!-- oracles:conformance:end -->";
        assert!(
            replace(same_line, "x")
                .unwrap_err()
                .contains("end its line")
        );
    }

    #[test]
    fn conformance_needs_our_report_and_matching_runs() {
        use test_suite::runner::CaseResult;
        use test_suite::standard::{CaseScore, Observation, Profile};
        let score = |expect_error| CaseScore {
            id: "A".into(),
            expect_error,
            panic: None,
            classified: true,
            events: Some(true),
            prefix_vendored: None,
            prefix_corrected: None,
            round_trip: Some(true),
            observation: Observation::default(),
        };
        let ours = StandardReport {
            name: "oneyaml-syntax",
            version: "0",
            profile: Some(Profile::FULL),
            cases: vec![score(false), score(true)],
        };
        let result = |expect_error| CaseResult {
            id: "A".into(),
            name: String::new(),
            expect_error,
            outcome: Ok(()),
            corrected: false,
        };
        let strict = Report {
            results: vec![result(false), result(true)],
        };
        let c = conformance("r".to_owned(), &ours, &strict).unwrap();
        assert_eq!(
            (c.valid, c.error, c.strict, c.round_trips),
            ((1, 1), (1, 1), (2, 2), 1)
        );
        let short = Report {
            results: vec![result(false)],
        };
        assert!(
            conformance("r".to_owned(), &ours, &short)
                .unwrap_err()
                .contains("strict run 1")
        );
        let other = StandardReport {
            name: "saphyr-parser",
            ..ours.clone()
        };
        assert!(conformance("r".to_owned(), &other, &strict).is_err());
        let no_events = StandardReport {
            profile: None,
            ..ours
        };
        assert!(conformance("r".to_owned(), &no_events, &strict).is_err());
    }

    /// `oracles --readme` never writes a conformance line naming an
    /// internal process identifier; a plain line is written.
    #[test]
    fn write_refuses_internal_identifiers() {
        // Under the target directory (`target/<profile>/deps/..`), not /tmp.
        let exe = std::env::current_exe().unwrap();
        let root = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(format!("oracles-readme-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/oneyaml")).unwrap();
        let doc = "- x\n  <!-- oracles:conformance:begin -->\n  old\n  \
                   <!-- oracles:conformance:end -->\n";
        let path = root.join(README);
        std::fs::write(&path, doc).unwrap();
        for bad in ["402/402 (DESIGN Q4.3)", "402/402 since unit 49b"] {
            let err = write(&root, bad).unwrap_err();
            assert!(err.contains("internal process identifier"), "{err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), doc);
        }
        assert_eq!(write(&root, "402/402").unwrap(), path);
        assert_eq!(
            current(&std::fs::read_to_string(&path).unwrap()).unwrap(),
            "402/402"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn release_tag() {
        assert_eq!(
            release(&test_suite::default_data_dir()).unwrap(),
            "data-2022-01-17"
        );
        let dir = workspace_root().join("crates/oracles/src");
        assert!(release(&dir).is_err());
    }
}
