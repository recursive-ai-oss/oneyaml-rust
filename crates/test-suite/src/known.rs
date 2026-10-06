//! Corrections for known defects in the vendored suite data: cases whose
//! recorded expected events no correct parser can produce.
//!
//! The list lives in `crates/test-suite/known-defects.txt`, one entry per
//! line: `<case-id> <correction> -- <reason>` (`#` starts a comment). A
//! correction replaces the case's defective `test.event` with the corrected
//! expectation, which is then **compared exactly**, like any other case:
//! nothing is excused, codes are checked as usual, and a real mismatch is a
//! real failure. The only correction is `events <e1>; <e2>; ...`: the
//! complete corrected expectation, one event per `;`-separated item. (A
//! blanket transform such as "add flow markers" cannot tell flow from block
//! collections without parsing the input, so it is deliberately absent.)
//!
//! [`KnownDefects::validate`] rejects an entry whose correction changes
//! nothing (the upstream data was fixed: the entry is stale).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::case::Case;

/// Why the list could not be loaded or validated.
#[derive(Debug)]
pub struct KnownDefectsError {
    path: PathBuf,
    line: usize,
    message: String,
}

impl core::fmt::Display for KnownDefectsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.line == 0 {
            write!(f, "{}: {}", self.path.display(), self.message)
        } else {
            write!(f, "{}:{}: {}", self.path.display(), self.line, self.message)
        }
    }
}

impl core::error::Error for KnownDefectsError {}

/// How a defective expectation is corrected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Correction {
    /// The full corrected expectation, one event per line.
    Events(String),
}

impl Correction {
    /// The corrected expectation (replacing the recorded one).
    #[must_use]
    pub fn apply(&self) -> String {
        match self {
            Self::Events(events) => events.clone(),
        }
    }
}

/// One entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    line: usize,
    correction: Correction,
    reason: String,
}

/// The known-defects list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownDefects {
    origin: PathBuf,
    by_case: BTreeMap<String, Entry>,
}

/// The shipped list (`crates/test-suite/known-defects.txt`).
#[must_use]
pub fn default_known_defects_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("known-defects.txt")
}

fn parse_correction(text: &str) -> Result<Correction, String> {
    let text = text.trim();
    let Some(events) = text.strip_prefix("events ") else {
        return Err(format!("unknown correction `{text}`"));
    };
    let mut out = String::new();
    for event in events.split(';').map(str::trim) {
        if event.is_empty() {
            return Err("empty event in correction".to_owned());
        }
        out.push_str(event);
        out.push('\n');
    }
    if !out.starts_with("+STR\n") {
        return Err("a corrected expectation starts with `+STR`".to_owned());
    }
    Ok(Correction::Events(out))
}

impl KnownDefects {
    /// An empty list (no case is corrected).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parses list text; `origin` is used in error messages only.
    ///
    /// # Errors
    ///
    /// A malformed entry, an entry without a reason, or a duplicate id.
    pub fn parse(text: &str, origin: &Path) -> Result<Self, KnownDefectsError> {
        let mut by_case = BTreeMap::new();
        for (n, raw) in text.lines().enumerate() {
            let line = n.saturating_add(1);
            let err = |message: String| KnownDefectsError {
                path: origin.to_path_buf(),
                line,
                message,
            };
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let (entry, reason) = content.split_once(" -- ").unwrap_or((content, ""));
            let (id, correction) = entry
                .trim()
                .split_once(char::is_whitespace)
                .unwrap_or((entry, ""));
            let reason = reason.trim();
            if reason.is_empty() {
                return Err(err(format!("known defect `{id}` has no ` -- reason`")));
            }
            let correction = parse_correction(correction).map_err(|m| err(format!("{id}: {m}")))?;
            let entry = Entry {
                line,
                correction,
                reason: reason.to_owned(),
            };
            if by_case.insert(id.to_owned(), entry).is_some() {
                return Err(err(format!("duplicate case `{id}`")));
            }
        }
        Ok(Self {
            origin: origin.to_path_buf(),
            by_case,
        })
    }

    /// Loads the list from `path`.
    ///
    /// # Errors
    ///
    /// I/O errors or a malformed list.
    pub fn load(path: &Path) -> Result<Self, KnownDefectsError> {
        let text = std::fs::read_to_string(path).map_err(|e| KnownDefectsError {
            path: path.to_path_buf(),
            line: 0,
            message: e.to_string(),
        })?;
        Self::parse(&text, path)
    }

    /// Checks every entry: it names a case of `cases`, and its correction
    /// changes that case's expectation (else the entry is stale).
    ///
    /// # Errors
    ///
    /// The first unknown or stale entry.
    pub fn validate(&self, cases: &[Case]) -> Result<(), KnownDefectsError> {
        for (id, entry) in &self.by_case {
            let err = |message: String| KnownDefectsError {
                path: self.origin.clone(),
                line: entry.line,
                message,
            };
            let case = cases
                .iter()
                .find(|c| c.id == *id)
                .ok_or_else(|| err(format!("unknown case `{id}`")))?;
            let corrected = entry.correction.apply();
            if corrected.lines().eq(case.expected_events.lines()) {
                return Err(err(format!(
                    "stale entry `{id}`: the correction changes nothing"
                )));
            }
        }
        Ok(())
    }

    /// Adds or replaces an entry.
    pub fn insert(
        &mut self,
        id: impl Into<String>,
        correction: Correction,
        reason: impl Into<String>,
    ) {
        let entry = Entry {
            line: 0,
            correction,
            reason: reason.into(),
        };
        self.by_case.insert(id.into(), entry);
    }

    /// The corrected expectation for `case`, if it is listed.
    #[must_use]
    pub fn corrected(&self, case: &Case) -> Option<String> {
        self.by_case.get(&case.id).map(|e| e.correction.apply())
    }

    /// Why `id` is listed, if it is.
    #[must_use]
    pub fn reason(&self, id: &str) -> Option<&str> {
        self.by_case.get(id).map(|e| e.reason.as_str())
    }

    /// The listed case ids, sorted.
    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.by_case.keys().map(String::as_str).collect()
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_case.len()
    }

    /// `true` when the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_case.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn case(id: &str, expected: &str) -> Case {
        Case {
            id: id.to_owned(),
            name: String::new(),
            input: Vec::new(),
            expected_events: expected.to_owned(),
            expect_error: true,
        }
    }

    fn err(text: &str) -> String {
        KnownDefects::parse(text, Path::new("k"))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn corrections() {
        let text = "# c\n\nA1 events +STR; +SEQ [] -- markers\nB2/01 events +STR; +DOC -- copied\n";
        let k = KnownDefects::parse(text, Path::new("k")).unwrap();
        assert_eq!(k.ids(), ["A1", "B2/01"]);
        assert_eq!(k.reason("A1"), Some("markers"));
        let a = case("A1", "+STR\n+SEQ\n");
        assert_eq!(k.corrected(&a).unwrap(), "+STR\n+SEQ []\n");
        let b = case("B2/01", "+STR\n+SEQ []\n");
        assert_eq!(k.corrected(&b).unwrap(), "+STR\n+DOC\n");
        assert_eq!(k.corrected(&case("C3", "+STR\n")), None);
        k.validate(&[a, b]).unwrap();
    }

    #[test]
    fn rejects_malformed_unknown_and_stale_entries() {
        assert_eq!(
            err("A1 events +STR\n"),
            "k:1: known defect `A1` has no ` -- reason`"
        );
        assert_eq!(err("A1 fix -- r\n"), "k:1: A1: unknown correction `fix`");
        assert_eq!(
            err("A1 events +DOC -- r\n"),
            "k:1: A1: a corrected expectation starts with `+STR`"
        );
        assert_eq!(
            err("A1 events +STR;; -- r\n"),
            "k:1: A1: empty event in correction"
        );
        assert_eq!(
            err("A1 events +STR -- r\nA1 events +STR -- s\n"),
            "k:2: duplicate case `A1`"
        );
        let k = KnownDefects::parse("A1 events +STR; +SEQ [] -- r\n", Path::new("k")).unwrap();
        assert_eq!(
            k.validate(&[case("A1", "+STR\n+SEQ []\n")])
                .unwrap_err()
                .to_string(),
            "k:1: stale entry `A1`: the correction changes nothing"
        );
        assert_eq!(
            k.validate(&[]).unwrap_err().to_string(),
            "k:1: unknown case `A1`"
        );
        let e = KnownDefects::load(Path::new("/nonexistent/k.txt")).unwrap_err();
        assert!(e.to_string().starts_with("/nonexistent/k.txt: "));
        let mut k = KnownDefects::empty();
        assert!(k.is_empty());
        k.insert("A1", Correction::Events("+STR\n".to_owned()), "r");
        assert_eq!(k.len(), 1);
    }
}
