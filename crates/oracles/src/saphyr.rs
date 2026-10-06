//! `saphyr-parser` event adapter.
//!
//! Its events carry the `---` flag but not `...`, no collection style, and
//! numeric anchor ids instead of names; see [`Saphyr::profile`].

use saphyr_parser::{Event, Parser, ScalarStyle, Tag};
use test_suite::standard::{Observation, Oracle, Profile};

use crate::{MAX_EVENTS, anchor_id, escape};

/// `saphyr-parser` (the parser under `saphyr`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Saphyr;

fn tag(t: Option<&Tag>) -> String {
    t.map(|t| crate::tag(&t.handle, &t.suffix))
        .unwrap_or_default()
}

fn line(event: &Event<'_>) -> Option<String> {
    Some(match event {
        Event::Nothing => return None,
        Event::StreamStart => "+STR".to_owned(),
        Event::StreamEnd => "-STR".to_owned(),
        Event::DocumentStart(true) => "+DOC ---".to_owned(),
        Event::DocumentStart(false) => "+DOC".to_owned(),
        Event::DocumentEnd => "-DOC".to_owned(),
        Event::Alias(id) => format!("=ALI *{id}"),
        Event::Scalar(value, style, id, t) => {
            let style = match style {
                ScalarStyle::Plain => ':',
                ScalarStyle::SingleQuoted => '\'',
                ScalarStyle::DoubleQuoted => '"',
                ScalarStyle::Literal => '|',
                ScalarStyle::Folded => '>',
            };
            let (a, t) = (anchor_id(*id), tag(t.as_deref()));
            format!("=VAL{a}{t} {style}{}", escape(value))
        }
        Event::SequenceStart(id, t) => format!("+SEQ{}{}", anchor_id(*id), tag(t.as_deref())),
        Event::SequenceEnd => "-SEQ".to_owned(),
        Event::MappingStart(id, t) => format!("+MAP{}{}", anchor_id(*id), tag(t.as_deref())),
        Event::MappingEnd => "-MAP".to_owned(),
    })
}

impl Oracle for Saphyr {
    fn name(&self) -> &'static str {
        "saphyr-parser"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn profile(&self) -> Option<Profile> {
        Some(Profile {
            doc_start_marker: true,
            doc_end_marker: false,
            flow_markers: false,
            anchor_names: false,
        })
    }

    fn observe(&mut self, input: &str) -> Observation {
        let mut lines = Vec::new();
        let mut error = None;
        for item in Parser::new_from_str(input).take(MAX_EVENTS) {
            match item {
                Ok((event, _)) => lines.extend(line(&event)),
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        // Neither an error nor StreamEnd: cut off at the event cap (C1).
        let truncated = error.is_none() && lines.last().map(String::as_str) != Some("-STR");
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
/// [`Saphyr::observe`]). Returns the number of events.
///
/// # Errors
/// saphyr-parser's error message, if it rejects the input.
pub fn drain(input: &str) -> Result<usize, String> {
    let mut count = 0usize;
    for item in Parser::new_from_str(input) {
        let (event, _) = item.map_err(|e| e.to_string())?;
        drop(event);
        count = count.saturating_add(1);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn renders_events() {
        let o = Saphyr.observe("--- &a !!str x\n");
        assert_eq!(o.error, None);
        assert_eq!(
            o.events.unwrap(),
            [
                "+STR",
                "+DOC ---",
                "=VAL &1 <tag:yaml.org,2002:str> :x",
                "-DOC",
                "-STR"
            ]
        );
        assert!(Saphyr.observe("[").error.is_some());
    }

    #[test]
    fn drain_counts_every_event_uncapped() {
        // +STR +DOC +SEQ =VAL =ALI -SEQ -DOC -STR
        assert_eq!(drain("- &a x\n- *a\n"), Ok(8));
        let many = "- x\n".repeat(crate::MAX_EVENTS);
        assert_eq!(drain(&many), Ok(crate::MAX_EVENTS + 6));
        assert!(drain("[").is_err());
    }
}
