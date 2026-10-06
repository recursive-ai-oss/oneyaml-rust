//! Markdown plumbing: number formatting, the section updater, and a small
//! ordered parallel map for running valgrind probes concurrently.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// `1234567` → `1,234,567`.
#[must_use]
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len().saturating_mul(4) / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len().saturating_sub(i)) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A markdown table header row plus its `|---|` separator.
#[must_use]
pub fn header(columns: &[String]) -> String {
    format!(
        "| {} |\n|{}\n",
        columns.join(" | "),
        "---|".repeat(columns.len())
    )
}

/// Runs `work` over `items` on `jobs` threads, keeping input order.
pub fn parallel<I: Sync, T: Send>(
    items: &[I],
    jobs: usize,
    work: impl Fn(&I) -> T + Sync,
) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<T>>> = Mutex::new(items.iter().map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..jobs.clamp(1, items.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let value = work(item);
                    if let Ok(mut guard) = out.lock()
                        && let Some(slot) = guard.get_mut(i)
                    {
                        *slot = Some(value);
                    }
                }
            });
        }
    });
    out.into_inner()
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .collect()
}

/// Replaces the body of section `name` in `doc` with `body`.
///
/// # Errors
/// If either marker is missing or they are out of order.
pub fn replace_section(doc: &str, name: &str, body: &str) -> Result<String, String> {
    let begin = format!("<!-- bench:{name}:begin -->\n");
    let end = format!("<!-- bench:{name}:end -->");
    let b = doc
        .find(&begin)
        .ok_or_else(|| format!("missing marker {begin:?}"))?;
    let body_start = b.saturating_add(begin.len());
    let e = doc
        .get(body_start..)
        .and_then(|rest| rest.find(&end))
        .map(|off| body_start.saturating_add(off))
        .ok_or_else(|| format!("missing marker {end:?} after {begin:?}"))?;
    let (head, tail) = (doc.get(..body_start), doc.get(e..));
    match (head, tail) {
        (Some(head), Some(tail)) => Ok(format!("{head}{body}{tail}")),
        _ => Err("marker offsets out of range".to_owned()),
    }
}

/// Regenerates `sections` of the markdown file at `path` in place, writing
/// after each one, with `build` producing a section's body.
///
/// # Errors
/// I/O errors, missing markers, or a failing `build`.
pub fn update(
    path: &Path,
    sections: &[&str],
    build: impl Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    let mut doc = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    for name in sections {
        let started = Instant::now();
        let body = build(name)?;
        doc = replace_section(&doc, name, &body)?;
        std::fs::write(path, &doc).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!(
            "updated `{name}` in {:.1} s",
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn header_row() {
        let cols = ["a".to_owned(), "b".to_owned()];
        assert_eq!(header(&cols), "| a | b |\n|---|---|\n");
    }

    #[test]
    fn replaces_only_between_markers() {
        let doc = "a\n<!-- bench:x:begin -->\nold\n<!-- bench:x:end -->\nz\n";
        let new = replace_section(doc, "x", "new\n").unwrap();
        assert_eq!(
            new,
            "a\n<!-- bench:x:begin -->\nnew\n<!-- bench:x:end -->\nz\n"
        );
        assert_eq!(replace_section(&new, "x", "new\n").unwrap(), new);
    }

    #[test]
    fn missing_markers_are_errors() {
        assert!(
            replace_section("nothing", "x", "")
                .unwrap_err()
                .contains("missing")
        );
        let only_begin = "<!-- bench:x:begin -->\n";
        assert!(replace_section(only_begin, "x", "").is_err());
        let reversed = "<!-- bench:x:end -->\n<!-- bench:x:begin -->\n";
        assert!(replace_section(reversed, "x", "").is_err());
    }

    #[test]
    fn update_rewrites_file_and_reports_errors() {
        // Under the target directory (`target/<profile>/deps/..`), not /tmp
        // (RAM-backed on the build host).
        let exe = std::env::current_exe().unwrap();
        let dir = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(format!("bench-md-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, "<!-- bench:x:begin -->\n<!-- bench:x:end -->\n").unwrap();
        update(&path, &["x"], |n| Ok(format!("{n}!\n"))).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "<!-- bench:x:begin -->\nx!\n<!-- bench:x:end -->\n"
        );
        assert!(update(&path, &["x"], |_| Err("boom".to_owned())).is_err());
        assert!(update(&path, &["y"], |_| Ok(String::new())).is_err());
        assert!(update(&dir.join("missing.md"), &["x"], |_| Ok(String::new())).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parallel_keeps_order() {
        let items: Vec<u32> = (0..50).collect();
        assert_eq!(
            parallel(&items, 8, |x| x * 2),
            items.iter().map(|x| x * 2).collect::<Vec<_>>()
        );
        assert!(parallel(&Vec::<u32>::new(), 4, |x| *x).is_empty());
    }
}
