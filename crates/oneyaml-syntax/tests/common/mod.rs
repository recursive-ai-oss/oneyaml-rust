//! Shared helpers for the syntax integration tests.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml_core::{Code, Diagnostic, Settings};
use oneyaml_syntax::event::{EventKind, EventList, ScalarStyle, check_contract};
use oneyaml_syntax::parse_with;

/// Parses under `settings`, asserting the contract and the byte tiling.
pub fn events<'a>(src: &'a str, settings: &Settings) -> EventList<'a> {
    let events = parse_with(src, settings);
    if let Err(err) = check_contract(src, &events) {
        panic!("{src:?}: {err}\n{events:#?}");
    }
    let rebuilt: String = events.sourced().map(|e| e.text().unwrap()).collect();
    assert_eq!(rebuilt.as_bytes(), src.as_bytes());
    events
}

/// Semantic events in compact notation (`+MAP`, `=VAL :x`, `!Y0006`,
/// `~Y0028`), joined by ` | `.
pub fn tree(src: &str, settings: &Settings) -> String {
    let mut out = Vec::new();
    for e in events(src, settings) {
        out.push(match e.kind() {
            EventKind::DocumentStart { .. } => "+DOC".to_owned(),
            EventKind::DocumentEnd { .. } => "-DOC".to_owned(),
            EventKind::MappingStart { .. } => "+MAP".to_owned(),
            EventKind::SequenceStart { .. } => "+SEQ".to_owned(),
            EventKind::MappingEnd => "-MAP".to_owned(),
            EventKind::SequenceEnd => "-SEQ".to_owned(),
            EventKind::Scalar { style, .. } => {
                let style = match style {
                    ScalarStyle::Plain => ':',
                    ScalarStyle::SingleQuoted => '\'',
                    ScalarStyle::DoubleQuoted => '"',
                    ScalarStyle::Literal => '|',
                    _ => '>',
                };
                let mut props = String::new();
                if let Some(a) = e.anchor() {
                    props.push_str(&format!("&{} ", a.name));
                }
                if let Some(t) = e.tag() {
                    props.push_str(&format!("<{}> ", t.resolved));
                }
                let value = e.value().unwrap();
                format!("=VAL {props}{style}{}", value.replace('\n', "\\n"))
            }
            EventKind::Alias => format!("=ALI *{}", e.alias_name().unwrap()),
            EventKind::Error(d) => format!("!{}", d.code),
            EventKind::Warning(d) => format!("~{}", d.code),
            _ => continue,
        });
    }
    out.join(" | ")
}

/// Every diagnostic, in stream order.
pub fn diagnostics(src: &str, settings: &Settings) -> Vec<Diagnostic> {
    events(src, settings)
        .iter()
        .filter_map(|e| e.diagnostic().cloned())
        .collect()
}

/// The first error's diagnostic.
pub fn first_error(src: &str, settings: &Settings) -> Option<Diagnostic> {
    events(src, settings)
        .iter()
        .find(|e| e.is_error())
        .and_then(|e| e.diagnostic().cloned())
}

/// The first error's code.
pub fn first_code(src: &str, settings: &Settings) -> Option<Code> {
    first_error(src, settings).map(|d| d.code)
}
