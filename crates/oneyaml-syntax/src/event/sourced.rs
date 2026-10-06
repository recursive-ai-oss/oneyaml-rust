//! Events paired with the source they were parsed from: the only public way
//! to read names and values (see *Reading names and values* in the module
//! docs).

use std::borrow::{Borrow, Cow};
use std::ops::Deref;

use super::{Anchor, DirectiveKind, Event, Tag, TagSource};

/// An event together with the source text it was parsed from.
///
/// Only the parser makes one: [`Events`](crate::parser::Events) and an
/// [`EventList`]'s owning iterator yield `Sourced<'a>` (owning its event),
/// and [`EventList::get_sourced`] and [`EventList::sourced`] lend
/// `Sourced<'a, &Event<'a>>` views. An event can
/// therefore never be read against the wrong text, which a bare
/// `(event, &str)` pair would allow silently.
///
/// ```
/// let src = "a: 1\n";
/// let list = oneyaml_syntax::parse(src);
/// let one = list.sourced().filter_map(|e| e.value()).nth(1);
/// assert_eq!(one.as_deref(), Some("1"));
/// ```
///
/// A bare event cannot be read against some other text:
///
/// ```compile_fail
/// let list = oneyaml_syntax::parse("a: 1\n");
/// let _ = list[4].value("b: 2\n");
/// ```
///
/// Dereferences to [`Event`] for everything that needs no source (kind,
/// span, properties' positions, diagnostics). What the readers return
/// borrows from the source, not from this value: `value()` of a
/// plain scalar outlives the `Sourced` it came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Sourced<'a, E = Event<'a>> {
    event: E,
    source: &'a str,
}

impl<'a, E: Borrow<Event<'a>>> Sourced<'a, E> {
    /// Pairs `event` with the source it was parsed from (parser only).
    pub(crate) const fn new(event: E, source: &'a str) -> Self {
        Self { event, source }
    }

    /// The event.
    #[must_use]
    #[doc(hidden)]
    pub fn event(&self) -> &Event<'a> {
        self.event.borrow()
    }

    /// The event, dropping the source (e.g. to store it).
    #[must_use]
    pub fn into_event(self) -> E {
        self.event
    }

    /// The whole source text the event was parsed from.
    #[must_use]
    pub const fn source(&self) -> &'a str {
        self.source
    }

    /// The source text this event covers (C1). `None` only for an event
    /// whose span is not on `char` boundaries, which the parser never
    /// produces.
    #[must_use]
    pub fn text(&self) -> Option<&'a str> {
        self.event().source(self.source)
    }

    /// A scalar's value after escape processing, line folding, and
    /// chomping. Borrowed from the source when the value is a slice of it.
    /// `None` for other events.
    #[must_use]
    pub fn value(&self) -> Option<Cow<'a, str>> {
        self.event().value(self.source)
    }

    /// The node's anchor (§6.9.2).
    #[must_use]
    pub fn anchor(&self) -> Option<Anchor<'a>> {
        self.event().anchor(self.source)
    }

    /// The node's tag, resolved (`%TAG` prefix and `%XX` escapes, §6.9.1).
    ///
    /// Cost: O(prefix + suffix) **per call**; the resolved text copies the
    /// `%TAG` prefix, which may be long. To read many tags, cache by
    /// [`TagSource::key`] ([`Sourced::tag_source`]) and resolve each
    /// distinct tag once.
    #[must_use]
    pub fn tag(&self) -> Option<Tag<'a>> {
        self.event().tag(self.source)
    }

    /// The node's tag as written, not yet resolved: O(the property's
    /// text), whatever the `%TAG` prefix's length.
    #[must_use]
    pub fn tag_source(&self) -> Option<TagSource<'a>> {
        self.event().tag_source(self.source)
    }

    /// An alias's anchor name, without `*`.
    #[must_use]
    pub fn alias_name(&self) -> Option<&'a str> {
        self.event().alias_name(self.source)
    }

    /// A directive's meaning.
    #[must_use]
    pub fn directive(&self) -> Option<DirectiveKind<'a>> {
        self.event().directive(self.source)
    }
}

impl<'a, E: Borrow<Event<'a>>> Deref for Sourced<'a, E> {
    type Target = Event<'a>;

    fn deref(&self) -> &Event<'a> {
        self.event.borrow()
    }
}

/// A collected event stream together with its source: what
/// [`parse`](crate::parser::parse) returns.
///
/// Dereferences to the slice of events for structural work (kinds, spans,
/// counting, [`check_contract`](super::check_contract)); reads go through
/// [`EventList::get_sourced`], [`EventList::sourced`], and the owning iterator,
/// which pair each event with this list's source.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventList<'a> {
    source: &'a str,
    events: Vec<Event<'a>>,
}

impl<'a> EventList<'a> {
    /// A list of events parsed from `source` (parser only).
    pub(crate) const fn new(source: &'a str, events: Vec<Event<'a>>) -> Self {
        Self { source, events }
    }

    /// Test tooling only, not part of the API: pairs `events` with `source`
    /// **without** checking that they were parsed from it, for harnesses
    /// that feed hand-built (even invalid) streams to their consumers.
    /// Reads of a mismatched pair return `None` or wrong text, never panic
    /// (never a panic). Library code gets lists only from the parser.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_parts_unchecked(source: &'a str, events: Vec<Event<'a>>) -> Self {
        Self { source, events }
    }

    /// The source text the events were parsed from.
    #[must_use]
    pub const fn source(&self) -> &'a str {
        self.source
    }

    /// The events, without their source.
    #[must_use]
    pub fn events(&self) -> &[Event<'a>] {
        &self.events
    }

    /// Event `index`, paired with the source.
    #[must_use]
    pub fn get_sourced(&self, index: usize) -> Option<Sourced<'a, &Event<'a>>> {
        self.events
            .get(index)
            .map(|event| Sourced::new(event, self.source))
    }

    /// The events in stream order, each paired with the source. (`iter()`,
    /// from the slice, yields the bare events.)
    pub fn sourced(&self) -> SourcedIter<'_, 'a> {
        SourcedIter {
            events: self.events.iter(),
            source: self.source,
        }
    }

    /// The events, dropping the source (e.g. to store them).
    #[must_use]
    pub fn into_events(self) -> Vec<Event<'a>> {
        self.events
    }
}

impl<'a> Deref for EventList<'a> {
    type Target = [Event<'a>];

    fn deref(&self) -> &[Event<'a>] {
        &self.events
    }
}

/// The events of an [`EventList`], each paired with the source (see
/// [`EventList::sourced`]).
#[derive(Clone, Debug)]
pub struct SourcedIter<'l, 'a> {
    events: core::slice::Iter<'l, Event<'a>>,
    source: &'a str,
}

impl<'l, 'a> Iterator for SourcedIter<'l, 'a> {
    type Item = Sourced<'a, &'l Event<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        let source = self.source;
        self.events.next().map(|event| Sourced::new(event, source))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.events.size_hint()
    }
}

impl DoubleEndedIterator for SourcedIter<'_, '_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let source = self.source;
        self.events
            .next_back()
            .map(|event| Sourced::new(event, source))
    }
}

impl ExactSizeIterator for SourcedIter<'_, '_> {}
impl core::iter::FusedIterator for SourcedIter<'_, '_> {}

impl<'a> IntoIterator for EventList<'a> {
    type Item = Sourced<'a>;
    type IntoIter = IntoSourced<'a>;

    fn into_iter(self) -> IntoSourced<'a> {
        IntoSourced {
            events: self.events.into_iter(),
            source: self.source,
        }
    }
}

/// The owning iterator of an [`EventList`]: its events, each paired with
/// the source.
#[derive(Clone, Debug)]
pub struct IntoSourced<'a> {
    events: std::vec::IntoIter<Event<'a>>,
    source: &'a str,
}

impl<'a> Iterator for IntoSourced<'a> {
    type Item = Sourced<'a>;

    fn next(&mut self) -> Option<Sourced<'a>> {
        let source = self.source;
        self.events.next().map(|event| Sourced::new(event, source))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.events.size_hint()
    }
}

impl DoubleEndedIterator for IntoSourced<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let source = self.source;
        self.events
            .next_back()
            .map(|event| Sourced::new(event, source))
    }
}

impl ExactSizeIterator for IntoSourced<'_> {}
impl core::iter::FusedIterator for IntoSourced<'_> {}
