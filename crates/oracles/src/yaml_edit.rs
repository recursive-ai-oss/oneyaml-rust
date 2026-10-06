//! `yaml-edit` adapter.
//!
//! `yaml-edit` has no event API, so nothing is mapped to events. What it does
//! expose is measured: whether `YamlFile::parse` reports errors (valid /
//! invalid classification) and whether its lossless tree prints back the
//! input byte for byte (round trip). Its parser is lenient by design and it
//! ships a separate strict `validator::Validator`, so it is scored twice:
//! parse errors alone, and parse errors plus error-severity violations
//! (warnings are ignored: they flag valid YAML).

use rowan::ast::AstNode;
use test_suite::standard::{Observation, Oracle, Profile};
/// yaml-edit's parsed file, for the `bench` crate's `document`/`edits`
/// sections, which hold one between steps (they reach yaml-edit only
/// through this crate).
pub use yaml_edit::YamlFile;
use yaml_edit::validator::{Severity, Validator};

/// `yaml-edit`; `validate` adds its strict [`Validator`].
#[derive(Clone, Copy, Debug, Default)]
pub struct YamlEdit {
    /// Also run `Validator::validate_syntax` on the whole tree.
    pub validate: bool,
}

impl Oracle for YamlEdit {
    fn name(&self) -> &'static str {
        if self.validate {
            "yaml-edit + Validator"
        } else {
            "yaml-edit"
        }
    }

    fn version(&self) -> &'static str {
        "0.3.2"
    }

    fn profile(&self) -> Option<Profile> {
        None
    }

    fn observe(&mut self, input: &str) -> Observation {
        let parsed = YamlFile::parse(input);
        let tree = parsed.tree();
        let mut error = parsed.errors().into_iter().next();
        if error.is_none() && self.validate {
            error = Validator::new()
                .validate_syntax(tree.syntax())
                .into_iter()
                .find(|v| v.severity == Severity::Error)
                .map(|v| format!("validator: {v}"));
        }
        let printed = tree.to_string();
        Observation {
            events: None,
            error,
            round_trip: Some(printed.as_bytes() == input.as_bytes()),
            truncated: false,
        }
    }
}

/// What [`drain`] saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drained {
    /// CST elements visited (nodes and tokens).
    pub elements: usize,
    /// `YamlFile::parse` reported at least one error.
    pub has_errors: bool,
}

/// Benchmark drain: `YamlFile::parse`, then a full preorder walk of the
/// lossless CST (every node and token), the closest equivalent of draining
/// a lossless event stream. No rendering, no validator.
#[must_use]
pub fn drain(input: &str) -> Drained {
    let parsed = YamlFile::parse(input);
    let has_errors = parsed.has_errors();
    let tree = parsed.tree();
    let elements = tree
        .syntax()
        .preorder_with_tokens()
        .filter(|e| matches!(e, rowan::WalkEvent::Enter(_)))
        .count();
    Drained {
        elements,
        has_errors,
    }
}

/// The edit benchmark's script (the `bench` crate's `edits`
/// section) on an already parsed file: in the first document's
/// root mapping, insert `key: "x"` (a new last entry), replace its value
/// with `"y"`, and remove it again. `false` when the root is not a
/// mapping or a step did not take. The file is left as it was found (up
/// to yaml-edit's own layout).
#[must_use]
pub fn edit_script(file: &YamlFile, key: &str) -> bool {
    let Some(doc) = file.document() else {
        return false;
    };
    doc.set(key, "x") && doc.set(key, "y") && doc.remove(key).is_some()
}

/// `YamlFile::parse` of `input`, or `None` when it reports an error: the
/// `document` benchmark's parse step (no walk, unlike [`drain`]).
#[must_use]
pub fn parse_checked(input: &str) -> Option<YamlFile> {
    let parsed = YamlFile::parse(input);
    (!parsed.has_errors()).then(|| parsed.tree())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn classifies_and_round_trips() {
        for validate in [false, true] {
            let o = YamlEdit { validate }.observe("a: b # c\n");
            assert_eq!((o.error, o.round_trip, o.events), (None, Some(true), None));
        }
        assert!(YamlEdit { validate: false }.observe("[").error.is_some());
    }

    #[test]
    fn drain_walks_the_whole_cst() {
        let small = drain("a: b # c\n");
        assert!(!small.has_errors);
        assert!(small.elements >= 5, "{small:?}");
        let big = drain(&"- x\n".repeat(1000));
        assert!(big.elements > 3000, "{big:?}");
        assert!(drain("[").has_errors);
    }

    #[test]
    fn edit_script_leaves_the_file_as_found() {
        let src = "a: 1 # c\nb:\n  - x\n";
        let file = parse_checked(src).unwrap();
        assert_eq!(file.to_string(), src);
        assert!(edit_script(&file, "zz"));
        assert_eq!(file.to_string(), src);
        assert!(!edit_script(&parse_checked("- 1\n").unwrap(), "zz"));
        assert!(parse_checked("[").is_none());
    }
}
