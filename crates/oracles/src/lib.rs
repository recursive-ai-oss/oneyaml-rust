//! Test-only differential oracles: other YAML implementations
//! scored on the vendored yaml-test-suite under the harness's **standard**
//! criterion ([`test_suite::standard`]).
//!
//! This is the only crate in the workspace allowed to depend on other YAML
//! implementations, including ones containing `unsafe` (`unsafe-libyaml`). It
//! is unpublished and no published crate depends on it. Its own code is
//! `deny(unsafe_code)`; the single exception is the FFI-style adapter in
//! [`libyaml`], which must call `unsafe-libyaml`'s `unsafe fn` API.
#![deny(unsafe_code)]

pub mod libyaml;
pub mod public_text;
pub mod readme;
pub mod report;

/// `docs/migrating-from-serde-yaml.md`, included here so that every code
/// example in it compiles and runs as a doc test:
/// `cargo test -p oracles --doc`.
#[doc = include_str!("../../../docs/migrating-from-serde-yaml.md")]
pub mod migration_guide {}
pub mod saphyr;
pub mod serde_cmp;
pub mod yaml_edit;
pub mod yaml_rust2;

use test_suite::standard::Oracle;

/// Every oracle, in table order (ours first).
#[must_use]
pub fn all() -> Vec<Box<dyn Oracle>> {
    vec![
        Box::new(test_suite::standard::ParserOracle),
        Box::new(saphyr::Saphyr),
        Box::new(yaml_rust2::YamlRust2),
        Box::new(libyaml::LibYaml),
        Box::new(yaml_edit::YamlEdit { validate: false }),
        Box::new(yaml_edit::YamlEdit { validate: true }),
    ]
}

/// Upper bound on events per input: a parser that never reaches its end
/// event is cut off (and then fails the case) rather than looping forever.
pub const MAX_EVENTS: usize = 100_000;

/// Escapes a scalar value the way `test.event` files do.
#[must_use]
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            '\u{8}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// ` &N` for a numeric anchor id (`0` means none), as saphyr/yaml-rust2 give.
#[must_use]
pub fn anchor_id(id: usize) -> String {
    if id == 0 {
        String::new()
    } else {
        format!(" &{id}")
    }
}

/// ` <handle+suffix>` for a resolved tag.
#[must_use]
pub fn tag(handle: &str, suffix: &str) -> String {
    format!(" <{handle}{suffix}>")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(escape("a\\\0\u{8}\n\r\tb"), "a\\\\\\0\\b\\n\\r\\tb");
        assert_eq!(anchor_id(0), "");
        assert_eq!(anchor_id(3), " &3");
        assert_eq!(tag("tag:yaml.org,2002:", "str"), " <tag:yaml.org,2002:str>");
    }

    #[test]
    fn names_are_unique() {
        let names: Vec<&str> = all().iter().map(|o| o.name()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }
}
