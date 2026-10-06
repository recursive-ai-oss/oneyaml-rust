//! The pluggable seam between the harness and an event producer.
//!
//! The real parser drops in by implementing [`EventProducer`] and
//! registering itself in [`by_name`]. [`StubProducer`] (the default) honestly
//! produces nothing; [`ParserProducer`] runs `oneyaml_syntax::parse`;
//! [`SettingsProducer`] runs `oneyaml_syntax::parse_with` under given
//! settings (`parser-lenient`, `parser-untrusted`).

use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::event::EventList;

/// The result of running a producer over one input.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Produced<'a> {
    /// The full event stream, including trivia and any error events, with
    /// the source it was parsed from.
    Events(EventList<'a>),
    /// The producer cannot handle inputs at all yet. Always scored as a failure.
    Unsupported,
}

/// Anything that turns YAML source text into our lossless event stream.
///
/// Error reporting is in-band: invalid input yields a stream containing at
/// least one [`EventKind::Error`](oneyaml_syntax::event::EventKind::Error)
/// event. There is deliberately no out-of-band "failed" result, so a producer
/// cannot score on invalid cases without emitting a real error event.
pub trait EventProducer {
    /// Short name used on the command line and in reports.
    fn name(&self) -> &'static str;

    /// Produces the event stream for `input`, borrowing from it.
    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a>;
}

/// Placeholder producer used until the parser exists. Returns
/// [`Produced::Unsupported`] for every input, so it scores 0 by construction.
#[derive(Clone, Copy, Debug, Default)]
pub struct StubProducer;

impl EventProducer for StubProducer {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn produce<'a>(&mut self, _input: &'a str) -> Produced<'a> {
        Produced::Unsupported
    }
}

/// The real parser: [`oneyaml_syntax::parse`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ParserProducer;

impl EventProducer for ParserProducer {
    fn name(&self) -> &'static str {
        "parser"
    }

    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        Produced::Events(oneyaml_syntax::parse(input))
    }
}

/// The real parser under explicit [`Settings`]: [`oneyaml_syntax::parse_with`].
#[derive(Clone, Debug)]
pub struct SettingsProducer {
    name: &'static str,
    settings: Settings,
}

impl SettingsProducer {
    /// A producer called `name` that parses under `settings`.
    #[must_use]
    pub const fn new(name: &'static str, settings: Settings) -> Self {
        Self { name, settings }
    }

    /// `parser-lenient`: `Settings::lenient_inline_colons(true)`.
    #[must_use]
    pub const fn lenient() -> Self {
        Self::new(
            "parser-lenient",
            Settings::new().lenient_inline_colons(true),
        )
    }

    /// `parser-untrusted`: `Limits::untrusted()`.
    #[must_use]
    pub const fn untrusted() -> Self {
        Self::new(
            "parser-untrusted",
            Settings::new().limits(Limits::untrusted()),
        )
    }
}

impl EventProducer for SettingsProducer {
    fn name(&self) -> &'static str {
        self.name
    }

    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        Produced::Events(oneyaml_syntax::parse_with(input, &self.settings))
    }
}

/// Names accepted by [`by_name`], default first.
pub const PRODUCER_NAMES: &[&str] = &["stub", "parser", "parser-lenient", "parser-untrusted"];

/// Looks up a registered producer by name.
#[must_use]
pub fn by_name(name: &str) -> Option<Box<dyn EventProducer>> {
    match name {
        "stub" => Some(Box::new(StubProducer)),
        "parser" => Some(Box::new(ParserProducer)),
        "parser-lenient" => Some(Box::new(SettingsProducer::lenient())),
        "parser-untrusted" => Some(Box::new(SettingsProducer::untrusted())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn registry() {
        for name in PRODUCER_NAMES {
            assert_eq!(by_name(name).unwrap().name(), *name);
        }
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn stub_is_unsupported() {
        assert_eq!(StubProducer.produce("a: b\n"), Produced::Unsupported);
    }
}
