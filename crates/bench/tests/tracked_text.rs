//! Every file in the repository reads as plain text for people who only
//! have the published repository.
//!
//! Source comments, tests, manifests, scripts, data headers and docs carry
//! no internal process identifiers (decision IDs, the private planning
//! documents, work-unit numbers, review jargon), as
//! [`oracles::public_text::internal_reference`] defines them, also when a
//! comment wraps between a word and its number, or in the lowercase
//! identifier forms `nn1`-`nn4` and `oi1`/`oi2`. Nor do they carry commit
//! hashes beyond one provenance line per recorded measurement. This scans
//! every file `git ls-files` lists (plus new files not yet added, as long
//! as `.gitignore` does not exclude them), and every such path:
//!
//! * identifiers in all text, and commit hashes in file paths;
//! * commit hashes (7 to 40 hex digits with a letter, or 7 to 12 digits)
//!   in comments, in Rust identifiers, in file names that Rust code
//!   mentions, and (7 to 12 characters, so table checksums stay out of
//!   scope) in Markdown. Rust string contents other than file names, and
//!   data rows, are not checked for hashes: they are test data, full of
//!   numbers and hex.
//!
//! The only lines that may hold one are listed in [`ALLOWED`] and
//! [`HASH_ALLOWED`], each with the reason; every entry must still match a
//! line, so the lists cannot go stale.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};
use std::process::Command;

use oracles::public_text::internal_reference;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

/// Files whose every line may hold an identifier: the detector itself,
/// whose docs and unit tests must spell out each form it finds.
const ALLOWED_FILES: &[&str] = &["crates/oracles/src/public_text.rs"];

/// Lines that may hold an identifier, as (file, a text that picks out the
/// line). The texts are chosen so that they hold no identifier themselves,
/// so this file needs no entry of its own.
const ALLOWED: &[(&str, &str)] = &[
    // Test fixtures of the generators' identifier refusal: a revision or
    // note naming a work unit or a decision must be refused.
    (
        "crates/bench/src/readme.rs",
        "(bench crate and docs only)\", None)",
    ),
    ("crates/bench/src/readme.rs", "audit tree\")),"),
    ("crates/bench/src/readme.rs", "for rev in [\"`4c6d70c` + "),
    (
        "crates/bench/src/readme.rs",
        "assert!(measured(&env(\"`a`\", Some(",
    ),
    (
        "crates/bench/src/readme.rs",
        "\"| CPU | CPU-value |\", \"| CPU | the ",
    ),
    (
        "crates/bench/src/readme.rs",
        "assert!(err.contains(\"\\\"NN",
    ),
    (
        "crates/bench/src/readme.rs",
        "replace(\"serde_yaml_ng\", \"OI",
    ),
    (
        "crates/bench/src/readme.rs",
        "assert!(err.contains(\"\\\"OI",
    ),
    (
        "crates/bench/src/readme.rs",
        "\"| Revision-value |\", \"| `abc` + ",
    ),
    ("crates/bench/src/report.rs", "for bad in [\"| target "),
    ("crates/oracles/src/readme.rs", "for bad in [\"402/402 ("),
    // Test fixtures of the generator source scan (`public_text.rs`).
    (
        "crates/bench/tests/public_text.rs",
        "in a doc comment\\nconst A: &str",
    ),
    (
        "crates/bench/tests/public_text.rs",
        "#[cfg(test)]\\nmod tests {",
    ),
    (
        "crates/bench/tests/public_text.rs",
        "(2, \"const A: &str = ",
    ),
    (
        "crates/bench/tests/public_text.rs",
        "assert_eq!(internal_reference(&lines[0].1)",
    ),
    // Test fixture of the path check.
    (
        "crates/bench/tests/tracked_text.rs",
        "assert!(path_hit(\"crates/oneyaml/tests/",
    ),
    // Not an identifier: a serde unit variant named `Unit` given content.
    (
        "crates/oneyaml/tests/tagged_variant_content.rs",
        "from_str_with::<Shape>(\"!Unit",
    ),
];

/// Lines that may hold a commit hash, as (file, a text that picks out the
/// line): one provenance line per recorded baseline or measurement,
/// upstream revisions of vendored and compared code, and examples.
const HASH_ALLOWED: &[(&str, &str)] = &[
    // Provenance of each recorded baseline.
    ("crates/bench/tests/doc_diagnostics.rs", "/// REV="),
    ("crates/bench/tests/ser_differential.rs", "/// REV="),
    ("crates/oneyaml/examples/edit_differential.rs", "//! REV="),
    (
        "crates/bench/data/edit-differential-baseline.tsv",
        "run on a `git archive` tree of commit",
    ),
    (
        "crates/test-suite/tests/event_baseline.rs",
        "hashes to the baseline recorded from commit",
    ),
    (
        "crates/test-suite/event-baseline.txt",
        "# Event-stream hashes per case, recorded at",
    ),
    // Provenance of the published measurements (generated).
    ("docs/conformance.md", "Measured on "),
    ("docs/performance.md", "| rustc | rustc "),
    ("crates/oneyaml/README.md", "- **Measured** "),
    // Upstream revisions: the yaml-test-suite data and libyaml.
    ("crates/oracles/tests/scores.rs", "// commit "),
    ("crates/test-suite/known-defects.txt", "branch head"),
    ("crates/test-suite/known-defects.txt", "on `main` ("),
    (
        "crates/test-suite/data/yaml-test-suite/SOURCE.md",
        "branch head, ",
    ),
    (
        "crates/test-suite/data/yaml-test-suite/SOURCE.md",
        "sources on `main` (",
    ),
    ("docs/conformance.md", "(release `data-2022-01-17`, commit"),
    (
        "docs/conformance.md",
        "c2rust translation of libyaml at commit",
    ),
    (
        "docs/conformance.md",
        "(2022-01-17), the same data we vendor",
    ),
    // Examples: how the generators write a measured revision, how numbers
    // are formatted, and a link-pinning fixture.
    (
        "crates/bench/src/readme.rs",
        "/// The measured revision, as written",
    ),
    (
        "crates/bench/src/readme.rs",
        "/// recorded (detected revisions are",
    ),
    (
        "crates/bench/src/readme.rs",
        "(SI, two significant figures)",
    ),
    ("crates/bench/src/markdown.rs", "/// `"),
    ("crates/bench/src/valgrind.rs", "/// `1,"),
    ("crates/bench/src/pin.rs", "[evidence]({REPO}/blob/"),
    // Test fixtures of the checks below.
    ("crates/bench/tests/tracked_text.rs", "hash_token(\"// at "),
    ("crates/bench/tests/tracked_text.rs", "hash_token(\"// on "),
    (
        "crates/bench/tests/tracked_text.rs",
        "path_hit(\"crates/bench/data/",
    ),
    ("crates/bench/tests/tracked_text.rs", "file_hit(\""),
];

/// Every file of the working tree git tracks or would track, that exists.
fn tracked(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .expect("git ls-files runs");
    assert!(out.status.success(), "git ls-files failed");
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| root.join(p).is_file())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// The comment text of `line`, if it has one: a `//` comment in a Rust
/// file (a `//` right after `:` is a URL), or a `#` line in a manifest,
/// script or data file. Markdown and YAML have no comments to check.
fn comment(file: &str, line: &str) -> Option<String> {
    let ext = file.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" => {
            let b = line.as_bytes();
            let at = (0..b.len().saturating_sub(1)).find(|&k| {
                b[k] == b'/'
                    && b[k.saturating_add(1)] == b'/'
                    && (k == 0 || b[k.saturating_sub(1)] != b':')
            })?;
            Some(
                line[at..]
                    .trim_start_matches('/')
                    .trim_start_matches('!')
                    .to_owned(),
            )
        }
        "toml" | "sh" | "txt" | "tsv" | "dict" | "py" => {
            let t = line.trim_start();
            t.strip_prefix('#').map(str::to_owned)
        }
        _ => None,
    }
}

/// Whether `seg` (one part of a name, between `_`, `-`, `/` or `.`) looks
/// like a commit hash: 7 to 40 hex digits with a letter and a digit, or 7
/// to 12 digits.
fn hash_like(seg: &str) -> bool {
    let hex = seg
        .bytes()
        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
    let digits = seg.bytes().all(|c| c.is_ascii_digit());
    let letter = seg.bytes().any(|c| c.is_ascii_lowercase());
    (hex && letter && digits_in(seg) && (7..=40).contains(&seg.len()))
        || (digits && (7..=12).contains(&seg.len()))
}

fn digits_in(seg: &str) -> bool {
    seg.bytes().any(|c| c.is_ascii_digit())
}

/// Whether `seg` is a lowercase decision or open-issue identifier.
fn short_id(seg: &str) -> bool {
    matches!(seg, "nn1" | "nn2" | "nn3" | "nn4" | "oi1" | "oi2")
}

/// The parts of `name` split at `_`, `-`, `/` and `.`.
fn segments(name: &str) -> impl Iterator<Item = &str> {
    name.split(['_', '-', '/', '.']).filter(|s| !s.is_empty())
}

/// A commit-hash-like token in `text`, standing alone (not part of a word,
/// a number like `1.5e10` or `1,234`, a `%`-escape, a path or a `0x`
/// literal), at most `max` characters long.
fn hash_token(text: &str, max: usize) -> Option<&str> {
    let b = text.as_bytes();
    let is_hex = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
    let mut i = 0;
    while i < b.len() {
        if !is_hex(b[i]) {
            i = i.saturating_add(1);
            continue;
        }
        let start = i;
        while i < b.len() && is_hex(b[i]) {
            i = i.saturating_add(1);
        }
        let tok = text.get(start..i).unwrap_or_default();
        let before = start.checked_sub(1).map(|k| b[k]);
        let after = b.get(i).copied();
        let glued =
            |c: Option<u8>| c.is_some_and(|c| c.is_ascii_alphanumeric() || b"_.,%-/$".contains(&c));
        if tok.len() <= max
            && hash_like(tok)
            && !glued(before)
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return Some(tok);
        }
    }
    None
}

/// Rust code of `line` without its comment and without the contents of
/// its string literals.
fn bare_code(line: &str) -> String {
    let mut out = String::new();
    let mut in_str = false;
    let mut prev = ' ';
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if !in_str && c == '/' && chars.peek() == Some(&'/') && prev != ':' {
            break;
        }
        if c == '"' && prev != '\\' {
            in_str = !in_str;
            out.push(c);
        } else if !in_str {
            out.push(c);
        }
        prev = if prev == '\\' && c == '\\' { ' ' } else { c };
    }
    out
}

/// A Rust identifier in `line` (outside comments and strings) with a
/// hash-like or short-identifier part.
fn ident_hit(line: &str) -> Option<String> {
    let code = bare_code(line);
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
        .find(|w| w.split('_').any(|s| hash_like(s) || short_id(s)))
        .map(str::to_owned)
}

/// A file name mentioned in `line` with a hash-like part.
fn file_hit(line: &str) -> Option<String> {
    const EXT: [&str; 8] = [
        ".rs",
        ".tsv",
        ".txt",
        ".toml",
        ".md",
        ".json",
        ".yaml",
        ".snapshot",
    ];
    line.split(|c: char| !(c.is_ascii_alphanumeric() || "_-/.".contains(c)))
        .filter_map(|w| EXT.iter().find_map(|e| w.strip_suffix(e)))
        .find(|stem| segments(stem).any(hash_like))
        .map(str::to_owned)
}

/// A file path with an identifier or a hash-like part.
fn path_hit(path: &str) -> Option<String> {
    let words: String = path
        .chars()
        .map(|c| if "/_.-".contains(c) { ' ' } else { c })
        .collect();
    internal_reference(&words).or_else(|| {
        segments(path)
            .find(|s| hash_like(s) || short_id(s))
            .map(str::to_owned)
    })
}

/// Whether `line` ends with "unit", "units" or "phase" and the comment
/// text of `next` starts with a digit: an identifier wrapped between lines.
fn split_reference(line: &str, next: &str) -> bool {
    let words = line.trim_end().to_ascii_lowercase();
    let ends = ["unit", "units", "phase"].iter().any(|w| {
        words.strip_suffix(w).is_some_and(|head| {
            head.chars()
                .next_back()
                .is_none_or(|c| !c.is_ascii_alphanumeric())
        })
    });
    let rest = next
        .trim_start()
        .trim_start_matches(['/', '!', '#'])
        .trim_start();
    ends && rest.starts_with(|c: char| c.is_ascii_digit())
}

#[test]
fn the_extra_checks_find_what_they_look_for() {
    assert_eq!(hash_token("// at 1a2b3c4d it was", 40), Some("1a2b3c4d"));
    assert_eq!(hash_token("// on 7654321 it took", 40), Some("7654321"));
    assert_eq!(
        hash_token(
            "1.00211145e-36, 0x1f90ab, %2521a, `cafe`, 1,234,567, 16 MiB",
            40
        ),
        None
    );
    assert_eq!(hash_token("deadbeef 0123456789abcdef", 12), None);
    assert_eq!(
        ident_hit("fn no_regression_against_1319da5() {").as_deref(),
        Some("no_regression_against_1319da5")
    );
    assert_eq!(ident_hit("use common::nn3;").as_deref(), Some("nn3"));
    assert_eq!(ident_hit("let x = \"1319da5 nn3\"; // nn3"), None);
    assert_eq!(
        file_hit("\"data/ser-baseline-f285bb1.tsv\"").as_deref(),
        Some("data/ser-baseline-f285bb1")
    );
    assert_eq!(file_hit("\"data/ser-baseline.tsv\""), None);
    assert!(path_hit("crates/oneyaml/tests/unit43.rs").is_some());
    assert!(path_hit("crates/bench/data/edit-differential-1319da5.tsv").is_some());
    assert!(path_hit("crates/oneyaml/tests/same_meaning.rs").is_none());
    for (line, next) in [("// a unit", "// 48d: x"), ("// (Phase", "// 6)")] {
        assert!(split_reference(line, next), "{line:?}");
    }
    assert!(!split_reference("// the unit", "// test that drives it"));
    assert!(!split_reference("// community", "// 5"));
}

#[test]
fn no_tracked_file_holds_an_internal_identifier() {
    let root = root();
    let files = tracked(&root);
    // The whole repository: the check must not pass vacuously.
    assert!(files.len() > 1000, "only {} files listed", files.len());
    let mut used = vec![false; ALLOWED.len()];
    let mut hash_used = vec![false; HASH_ALLOWED.len()];
    let mut found = Vec::new();
    for file in &files {
        // Fuzz corpora are generated inputs named by their content's hash.
        if file.starts_with("crates/fuzz/corpus/") {
            continue;
        }
        if let Some(id) = path_hit(file) {
            found.push(format!("{file}: {id:?} in the path"));
        }
        if ALLOWED_FILES.contains(&file.as_str()) {
            continue;
        }
        let bytes = std::fs::read(root.join(file)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let rust = file.ends_with(".rs");
        let markdown = file.ends_with(".md");
        for (n, line) in lines.iter().enumerate() {
            let at = format!("{file}:{}", n.saturating_add(1));
            let next = lines.get(n.saturating_add(1)).copied().unwrap_or("");
            let id = internal_reference(line)
                .or_else(|| {
                    split_reference(line, next).then(|| "a wrapped unit or phase number".to_owned())
                })
                .or_else(|| {
                    rust.then(|| ident_hit(line))
                        .flatten()
                        .filter(|w| !w.split('_').any(hash_like))
                });
            if let Some(id) = id {
                match ALLOWED
                    .iter()
                    .position(|(f, pick)| f == file && line.contains(pick))
                {
                    Some(i) => used[i] = true,
                    None => found.push(format!("{at}: {id:?}")),
                }
            }
            let hash = comment(file, line)
                .as_deref()
                .and_then(|c| hash_token(c, 40))
                .map(str::to_owned)
                .or_else(|| {
                    markdown
                        .then(|| hash_token(line, 12).map(str::to_owned))
                        .flatten()
                })
                .or_else(|| {
                    rust.then(|| {
                        ident_hit(line)
                            .filter(|w| w.split('_').any(hash_like))
                            .or_else(|| file_hit(line))
                    })
                    .flatten()
                });
            if let Some(tok) = hash {
                match HASH_ALLOWED
                    .iter()
                    .position(|(f, pick)| f == file && line.contains(pick))
                {
                    Some(i) => hash_used[i] = true,
                    None => found.push(format!("{at}: commit hash {tok:?}")),
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "internal identifiers in tracked files:\n{}",
        found.join("\n")
    );
    let stale: Vec<_> = ALLOWED
        .iter()
        .zip(&used)
        .chain(HASH_ALLOWED.iter().zip(&hash_used))
        .filter(|(_, u)| !**u)
        .map(|(a, _)| format!("{a:?}"))
        .collect();
    assert!(
        stale.is_empty(),
        "allowlist entries that match no line:\n{}",
        stale.join("\n")
    );
}
