//! Per-case expected diagnostic codes: the hook that lets the harness
//! check *which* errors and warnings a producer reports, not just that it
//! reports one.
//!
//! The table lives in `crates/test-suite/expected-codes.txt`. Each non-comment
//! line is `<case-id> <Y####>...`: the codes of every `Error` and `Warning`
//! event the producer must emit for that case, **in stream order**. A case
//! with no line is not checked for codes. A line with an id but no codes
//! asserts that the case produces no diagnostics at all.
//!
//! [`ExpectedCodes::validate`] ties the table to the registry and the suite:
//! every id is a known case, every code is registered and not deprecated, a
//! valid case lists only warning codes, and an error case lists at least one
//! error code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oneyaml_core::{Code, Severity};

use crate::case::Case;

/// Why the table could not be loaded.
#[derive(Debug)]
pub struct CodesError {
    path: PathBuf,
    line: usize,
    message: String,
}

impl core::fmt::Display for CodesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.line == 0 {
            write!(f, "{}: {}", self.path.display(), self.message)
        } else {
            write!(f, "{}:{}: {}", self.path.display(), self.line, self.message)
        }
    }
}

impl core::error::Error for CodesError {}

/// The expected-codes table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpectedCodes {
    origin: PathBuf,
    /// Case id -> (line number, codes).
    by_case: BTreeMap<String, (usize, Vec<Code>)>,
}

/// The shipped table (`crates/test-suite/expected-codes.txt`).
#[must_use]
pub fn default_codes_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("expected-codes.txt")
}

impl ExpectedCodes {
    /// An empty table (no case is checked for codes).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parses table text; `origin` is used in error messages only.
    ///
    /// # Errors
    ///
    /// Malformed codes or duplicate case ids, with their line number.
    pub fn parse(text: &str, origin: &Path) -> Result<Self, CodesError> {
        let mut by_case = BTreeMap::new();
        for (n, raw) in text.lines().enumerate() {
            let line_no = n.saturating_add(1);
            let err = |message: String| CodesError {
                path: origin.to_path_buf(),
                line: line_no,
                message,
            };
            let line = raw.split('#').next().unwrap_or("").trim();
            let mut words = line.split_whitespace();
            let Some(id) = words.next() else { continue };
            let mut codes = Vec::new();
            for word in words {
                codes.push(Code::parse(word).ok_or_else(|| err(format!("bad code `{word}`")))?);
            }
            if by_case.insert(id.to_owned(), (line_no, codes)).is_some() {
                return Err(err(format!("duplicate case `{id}`")));
            }
        }
        Ok(Self {
            origin: origin.to_path_buf(),
            by_case,
        })
    }

    /// Checks every entry against the code registry and `cases`.
    ///
    /// # Errors
    ///
    /// The first entry naming an unknown case, an unregistered or deprecated
    /// code, an error code on a valid case, or no error code on an error case.
    pub fn validate(&self, cases: &[Case]) -> Result<(), CodesError> {
        for (id, (line, codes)) in &self.by_case {
            let err = |message: String| CodesError {
                path: self.origin.clone(),
                line: *line,
                message,
            };
            let case = cases
                .iter()
                .find(|c| c.id == *id)
                .ok_or_else(|| err(format!("unknown case `{id}`")))?;
            let mut has_error = false;
            for code in codes {
                let info = code
                    .info()
                    .ok_or_else(|| err(format!("{code} is not in the code registry")))?;
                if info.deprecated {
                    return Err(err(format!("{code} is deprecated")));
                }
                if info.severity == Severity::Error {
                    has_error = true;
                    if !case.expect_error {
                        return Err(err(format!("valid case `{id}` lists error code {code}")));
                    }
                }
            }
            if case.expect_error && !has_error {
                return Err(err(format!("error case `{id}` lists no error code")));
            }
        }
        Ok(())
    }

    /// Loads the table from `path`.
    ///
    /// # Errors
    ///
    /// I/O errors or a malformed table.
    pub fn load(path: &Path) -> Result<Self, CodesError> {
        let text = std::fs::read_to_string(path).map_err(|e| CodesError {
            path: path.to_path_buf(),
            line: 0,
            message: e.to_string(),
        })?;
        Self::parse(&text, path)
    }

    /// Adds or replaces the expectation for one case.
    pub fn insert(&mut self, id: impl Into<String>, codes: Vec<Code>) {
        self.by_case.insert(id.into(), (0, codes));
    }

    /// The expected codes for `id`, if the table has an entry.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&[Code]> {
        self.by_case.get(id).map(|(_, codes)| codes.as_slice())
    }

    /// Number of cases with an entry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_case.len()
    }

    /// `true` when no case has an entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_case.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use oneyaml_core::code::RESERVED_DIRECTIVE_IGNORED;

    #[test]
    fn parses_entries_comments_and_empty_lists() {
        let t = ExpectedCodes::parse(
            "# header\n\n6LVF Y1003  # reserved directive\n2G84/00 Y0001 Y0002\nD88J\n",
            Path::new("t"),
        )
        .unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t.get("6LVF"), Some(&[RESERVED_DIRECTIVE_IGNORED][..]));
        assert_eq!(t.get("2G84/00").unwrap().len(), 2);
        assert_eq!(t.get("D88J"), Some(&[][..]));
        assert_eq!(t.get("NOPE"), None);
    }

    #[test]
    fn rejects_bad_codes_and_duplicates() {
        let e = ExpectedCodes::parse("A Y0000\n", Path::new("t")).unwrap_err();
        assert_eq!(e.to_string(), "t:1: bad code `Y0000`");
        let e = ExpectedCodes::parse("A\nA Y0001\n", Path::new("t")).unwrap_err();
        assert_eq!(e.to_string(), "t:2: duplicate case `A`");
    }

    fn case(id: &str, expect_error: bool) -> Case {
        Case {
            id: id.to_owned(),
            name: String::new(),
            input: Vec::new(),
            expected_events: String::new(),
            expect_error,
        }
    }

    fn check(table: &str, cases: &[Case]) -> Result<(), String> {
        ExpectedCodes::parse(table, Path::new("t"))
            .unwrap()
            .validate(cases)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn validation_rules() {
        let cases = [case("V", false), case("E", true)];
        assert_eq!(
            check("V Y1003\nV2", &cases),
            Err("t:2: unknown case `V2`".into())
        );
        assert_eq!(check("V Y1003", &cases), Ok(()));
        assert_eq!(check("V", &cases), Ok(()));
        assert_eq!(
            check("V Y0999", &cases),
            Err("t:1: Y0999 is not in the code registry".into())
        );
        assert_eq!(
            check("V Y1002", &cases),
            Err("t:1: valid case `V` lists error code Y1002".into())
        );
        assert_eq!(
            check("E Y1003", &cases),
            Err("t:1: error case `E` lists no error code".into())
        );
        assert_eq!(check("E Y1003 Y1002", &cases), Ok(()));
    }

    #[test]
    fn load_and_insert() {
        let e = ExpectedCodes::load(Path::new("/nonexistent/codes.txt")).unwrap_err();
        assert!(e.to_string().starts_with("/nonexistent/codes.txt: "));
        let mut t = ExpectedCodes::empty();
        assert!(t.is_empty());
        t.insert("X", vec![RESERVED_DIRECTIVE_IGNORED]);
        assert!(!t.is_empty());
        assert_eq!(t.get("X"), Some(&[RESERVED_DIRECTIVE_IGNORED][..]));
    }
}
