//! `yaml-rust2` event adapter.
//!
//! Its events carry neither `---` nor `...`, no collection style, and numeric
//! anchor ids instead of names; see [`YamlRust2::profile`].

use test_suite::standard::{Observation, Oracle, Profile};
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::TScalarStyle;

use crate::{MAX_EVENTS, anchor_id, escape};

/// `yaml-rust2`.
#[derive(Clone, Copy, Debug, Default)]
pub struct YamlRust2;

fn tag(t: Option<&Tag>) -> String {
    t.map(|t| crate::tag(&t.handle, &t.suffix))
        .unwrap_or_default()
}

fn line(event: &Event) -> Option<String> {
    Some(match event {
        Event::Nothing => return None,
        Event::StreamStart => "+STR".to_owned(),
        Event::StreamEnd => "-STR".to_owned(),
        Event::DocumentStart => "+DOC".to_owned(),
        Event::DocumentEnd => "-DOC".to_owned(),
        Event::Alias(id) => format!("=ALI *{id}"),
        Event::Scalar(value, style, id, t) => {
            let style = match style {
                TScalarStyle::Plain => ':',
                TScalarStyle::SingleQuoted => '\'',
                TScalarStyle::DoubleQuoted => '"',
                TScalarStyle::Literal => '|',
                TScalarStyle::Folded => '>',
            };
            let (a, t) = (anchor_id(*id), tag(t.as_ref()));
            format!("=VAL{a}{t} {style}{}", escape(value))
        }
        Event::SequenceStart(id, t) => format!("+SEQ{}{}", anchor_id(*id), tag(t.as_ref())),
        Event::SequenceEnd => "-SEQ".to_owned(),
        Event::MappingStart(id, t) => format!("+MAP{}{}", anchor_id(*id), tag(t.as_ref())),
        Event::MappingEnd => "-MAP".to_owned(),
    })
}

impl Oracle for YamlRust2 {
    fn name(&self) -> &'static str {
        "yaml-rust2"
    }

    fn version(&self) -> &'static str {
        "0.13.0"
    }

    fn profile(&self) -> Option<Profile> {
        Some(Profile {
            doc_start_marker: false,
            doc_end_marker: false,
            flow_markers: false,
            anchor_names: false,
        })
    }

    fn observe(&mut self, input: &str) -> Observation {
        let mut parser = Parser::new_from_str(input);
        let mut lines = Vec::new();
        let mut error = None;
        // Cleared when the parser reaches StreamEnd or fails (C1).
        let mut truncated = true;
        for _ in 0..MAX_EVENTS {
            match parser.next_token() {
                Ok((event, _)) => {
                    let end = matches!(event, Event::StreamEnd);
                    lines.extend(line(&event));
                    if end {
                        truncated = false;
                        break;
                    }
                }
                Err(e) => {
                    error = Some(e.to_string());
                    truncated = false;
                    break;
                }
            }
        }
        Observation {
            events: Some(lines),
            error,
            round_trip: None,
            truncated,
        }
    }
}

/// Benchmark drain: pulls every event to `StreamEnd`, with no rendering,
/// no per-event allocation by the caller and no event cap (unlike
/// [`YamlRust2::observe`]). Returns the number of events.
///
/// # Errors
/// yaml-rust2's error message, if it rejects the input.
pub fn drain(input: &str) -> Result<usize, String> {
    let mut parser = Parser::new_from_str(input);
    let mut count = 0usize;
    loop {
        let (event, _) = parser.next_token().map_err(|e| e.to_string())?;
        count = count.saturating_add(1);
        if matches!(event, Event::StreamEnd) {
            return Ok(count);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn renders_events() {
        let o = YamlRust2.observe("- &a 'x'\n- *a\n");
        assert_eq!(o.error, None);
        assert_eq!(
            o.events.unwrap(),
            [
                "+STR",
                "+DOC",
                "+SEQ",
                "=VAL &1 'x",
                "=ALI *1",
                "-SEQ",
                "-DOC",
                "-STR"
            ]
        );
        assert!(YamlRust2.observe("[").error.is_some());
    }

    #[test]
    fn drain_counts_every_event_uncapped() {
        assert_eq!(drain("- &a x\n- *a\n"), Ok(8));
        let many = "- x\n".repeat(crate::MAX_EVENTS);
        assert_eq!(drain(&many), Ok(crate::MAX_EVENTS + 6));
        assert!(drain("[").is_err());
    }
}
