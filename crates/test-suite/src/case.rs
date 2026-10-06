//! Loading yaml-test-suite cases from the vendored data directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One yaml-test-suite case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    /// Case id: `XXXX` for single cases, `XXXX/NN` for sub-cases.
    pub id: String,
    /// Human-readable name from the `===` file.
    pub name: String,
    /// Raw bytes of `in.yaml`.
    pub input: Vec<u8>,
    /// Contents of `test.event` (the canonical event tree).
    pub expected_events: String,
    /// Whether the case is invalid YAML (an `error` file is present).
    pub expect_error: bool,
}

impl Case {
    /// Whether `filter` selects this case: an exact id match, or a top-level
    /// id (`2G84`) selecting all of its sub-cases (`2G84/00`, …).
    #[must_use]
    pub fn matches(&self, filter: &str) -> bool {
        if self.id == filter {
            return true;
        }
        self.id
            .strip_prefix(filter)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// Failure to load the suite.
#[derive(Debug)]
pub struct LoadError {
    path: PathBuf,
    kind: LoadErrorKind,
}

#[derive(Debug)]
enum LoadErrorKind {
    Io(io::Error),
    NotUtf8,
    NoCases,
}

impl LoadError {
    fn io(path: &Path, err: io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: LoadErrorKind::Io(err),
        }
    }

    /// The file or directory that could not be loaded.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let path = self.path.display();
        match &self.kind {
            LoadErrorKind::Io(err) => write!(f, "{path}: {err}"),
            LoadErrorKind::NotUtf8 => write!(f, "{path}: not valid UTF-8"),
            LoadErrorKind::NoCases => write!(f, "{path}: no test cases found"),
        }
    }
}

impl core::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match &self.kind {
            LoadErrorKind::Io(err) => Some(err),
            LoadErrorKind::NotUtf8 | LoadErrorKind::NoCases => None,
        }
    }
}

/// The vendored data directory (`crates/test-suite/data/yaml-test-suite`).
#[must_use]
pub fn default_data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("yaml-test-suite")
}

/// Loads every case under `root`, sorted by id.
///
/// The layout is fixed at two levels (`XXXX/` or `XXXX/NN/`), so this walks it
/// with two loops rather than recursively.
///
/// # Errors
///
/// Fails on I/O errors, non-UTF-8 `===`/`test.event` files, or when `root`
/// contains no cases.
pub fn load_suite(root: &Path) -> Result<Vec<Case>, LoadError> {
    let mut cases = Vec::new();
    for top in sorted_subdirs(root)? {
        let top_id = file_name(&top);
        if top.join("in.yaml").is_file() {
            cases.push(load_case(&top, top_id.clone())?);
            continue;
        }
        for sub in sorted_subdirs(&top)? {
            if sub.join("in.yaml").is_file() {
                let id = format!("{top_id}/{}", file_name(&sub));
                cases.push(load_case(&sub, id)?);
            }
        }
    }
    if cases.is_empty() {
        return Err(LoadError {
            path: root.to_path_buf(),
            kind: LoadErrorKind::NoCases,
        });
    }
    Ok(cases)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn sorted_subdirs(dir: &Path) -> Result<Vec<PathBuf>, LoadError> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| LoadError::io(dir, e))? {
        let entry = entry.map_err(|e| LoadError::io(dir, e))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn read_text(path: &Path) -> Result<String, LoadError> {
    let bytes = fs::read(path).map_err(|e| LoadError::io(path, e))?;
    String::from_utf8(bytes).map_err(|_| LoadError {
        path: path.to_path_buf(),
        kind: LoadErrorKind::NotUtf8,
    })
}

fn load_case(dir: &Path, id: String) -> Result<Case, LoadError> {
    let in_yaml = dir.join("in.yaml");
    let input = fs::read(&in_yaml).map_err(|e| LoadError::io(&in_yaml, e))?;
    let name = read_text(&dir.join("==="))?.trim().to_owned();
    let expected_events = read_text(&dir.join("test.event"))?;
    let expect_error = dir.join("error").is_file();
    Ok(Case {
        id,
        name,
        input,
        expected_events,
        expect_error,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn case(id: &str) -> Case {
        Case {
            id: id.to_owned(),
            name: String::new(),
            input: Vec::new(),
            expected_events: String::new(),
            expect_error: false,
        }
    }

    #[test]
    fn filter_matching() {
        assert!(case("2G84/00").matches("2G84/00"));
        assert!(case("2G84/00").matches("2G84"));
        assert!(!case("2G84/00").matches("2G8"));
        assert!(!case("2G84/00").matches("2G84/01"));
        assert!(case("D88J").matches("D88J"));
        assert!(!case("D88J").matches("D88"));
    }

    #[test]
    fn missing_dir_is_an_error() {
        let err = load_suite(Path::new("/nonexistent/oneyaml/suite")).unwrap_err();
        assert!(err.to_string().contains("/nonexistent/oneyaml/suite"));
        assert!(core::error::Error::source(&err).is_some());
        assert_eq!(err.path(), Path::new("/nonexistent/oneyaml/suite"));
    }

    #[test]
    fn empty_dir_has_no_cases() {
        let dir = std::env::temp_dir().join(format!("oneyaml-empty-suite-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let err = load_suite(&dir).unwrap_err();
        fs::remove_dir_all(&dir).unwrap();
        assert!(err.to_string().ends_with("no test cases found"));
        assert!(core::error::Error::source(&err).is_none());
    }

    #[test]
    fn non_utf8_event_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("oneyaml-bad-suite-{}", std::process::id()));
        let case_dir = dir.join("AAAA");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join("in.yaml"), b"a\n").unwrap();
        fs::write(case_dir.join("==="), b"bad").unwrap();
        fs::write(case_dir.join("test.event"), [0xff, 0xfe]).unwrap();
        let err = load_suite(&dir).unwrap_err();
        fs::remove_dir_all(&dir).unwrap();
        assert!(err.to_string().ends_with("not valid UTF-8"));
    }
}
