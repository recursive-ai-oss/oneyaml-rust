//! The serde deserializer.
//!
//! It reads the **same** event stream as every other consumer, through the
//! same resolver ([`crate::resolve::Resolver`]) and the same construction
//! layer ([`crate::construct`]): aliases are expanded by the shared
//! [`Expander`](crate::construct::expand::Expander), `Value`s are built by
//! the shared builder, and duplicate and merge keys follow the shared
//! rules.
//!
//! # Recursion
//!
//! serde's data model is recursive: a `Visitor` deserializes children by
//! calling back into the deserializer. So the native stack grows by one
//! (small, fixed) group of frames per nesting level **of the target type
//! that is actually visited**. That depth is bounded by `max_depth` (1024
//! by default): the parser rejects deeper input, and the expander rejects
//! alias expansions that would nest deeper (`Y2000`). Everything that is
//! *not* dictated by the target type is iterative: checking and skipping
//! ignored content, building `Value`s (`from_str::<Value>` and `Value`
//! fields never recurse), planning mappings for tolerated duplicates and
//! the values of merge keys (one explicit-stack pass per outermost buffered
//! mapping or `<<` value, nested merge sources included, see `plan`),
//! alias replay, and dropping.
//!
//! # Agreement with `Value`
//!
//! Content the target does not consume (`IgnoredAny`, unknown struct
//! fields, the rest of a fixed-size tuple, unread documents) is still
//! resolved and checked for duplicate and merge keys, exactly as the
//! `Value` builder would, so a typed read never accepts input that
//! `from_str::<Value>` rejects.
//!
//! Only a *recursive user type* (e.g. `struct Tree(Vec<Tree>)`) makes that
//! depth follow the input, so it is bounded separately by
//! `Limits::max_typed_depth` (128 by default, 64 untrusted): each
//! collection visited through a typed visitor counts one level, and one
//! level more is `Y2007`. At ~1.9 KiB per level in release and ~7.4 KiB in
//! debug builds, the default fits a 2 MiB thread stack. With
//! `Limits::unlimited()` there is no bound; that is the caller's explicit
//! choice.

use std::collections::VecDeque;
use std::num::NonZeroU32;

use oneyaml_core::{DuplicateKeys, Settings};

use crate::construct::{Ev, Node, Pull};
use crate::error::Error;
use crate::resolve::Resolver;

mod access;
mod api;
mod core;
mod plan;
pub(crate) mod scalar;
mod value;

pub(crate) use api::read_discarding;
pub use api::{
    Deserializer, from_reader, from_reader_with, from_slice, from_slice_with, from_str,
    from_str_with, from_str_with_diagnostics,
};
pub(crate) use plan::check_unexpanded;
pub use value::{from_value, from_value_with};

/// What the target type asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hint {
    Any,
    Ignored,
    Bool,
    Int(scalar::IntTy),
    Float,
    /// `str`, `String`, `char`, bytes, identifiers.
    Str,
    Option,
    Unit,
    Newtype,
    /// Sequences and tuples.
    Seq,
    /// Maps and structs.
    Map,
    /// An enum, with its variant names (a `%TAG` tag that names
    /// one is matched without copying its prefix).
    Enum(&'static [&'static str]),
}

/// The event source of a [`Core`]: events pushed back or rewritten (merge
/// planning) first, then the pull source. Queued events carry whether they
/// were already planned, so each mapping is planned once, and the
/// typed level a re-entry already paid for the node they start.
pub(crate) struct Src<'de, P> {
    p: P,
    /// The level is a `NonZeroU32` (levels start at 1) so an entry stays as
    /// small as with a plain flag (allocation bytes: merge planning
    /// queues whole mappings); a level past `u32::MAX` is never marked,
    /// so it is charged again (never a free pass).
    queue: VecDeque<(Ev<'de>, bool, Option<NonZeroU32>)>,
    last_end: usize,
    /// The last event returned by `next` came from planned output.
    last_planned: bool,
    /// The last event returned by `next` was pushed back by a re-entry
    /// (an option, a newtype, `Spanned`) that raised the typed depth to
    /// this level. Honoured only at that level (a re-entry whose
    /// visitor never reads the node must not leave a free pass for a
    /// reader outside it).
    last_prepaid: Option<NonZeroU32>,
    /// Collections whose start was read and whose end was not (events
    /// pushed back count as unread): whether a failed read left the rest
    /// of a collection unread.
    open: usize,
    /// Events read so far (wrapping): whether a read took anything.
    reads: usize,
    /// Where the read was poisoned: a target ignored
    /// an error it may not ignore (the input's own, or a collection whose
    /// rest could not be read), or the event source failed. From then on
    /// every read of an event is `Y3018`, so no reader — an accessor, a
    /// node, the rest of a collection after a visitor returned — reads on
    /// out of step (never `Y9000`, never a value for invalid input).
    poisoned: Option<oneyaml_core::Span>,
}

/// `+1` for a collection start, `-1` for an end, `0` otherwise.
fn nesting(ev: &Ev<'_>) -> isize {
    match ev.node {
        Node::SeqStart { .. } | Node::MapStart { .. } => 1,
        Node::SeqEnd | Node::MapEnd => -1,
        _ => 0,
    }
}

impl<P> Src<'_, P> {
    /// Counts `ev` as read (`by` = 1) or pushed back (`by` = -1).
    fn count(&mut self, ev: &Ev<'_>, by: isize) {
        self.open = self
            .open
            .saturating_add_signed(nesting(ev).saturating_mul(by));
        self.reads = if by > 0 {
            self.reads.wrapping_add(1)
        } else {
            self.reads.wrapping_sub(1)
        };
    }
}

impl<P> Src<'_, P> {
    /// `Y3018` once poisoned.
    fn check_unpoisoned(&self) -> Result<(), Error> {
        match self.poisoned {
            None => Ok(()),
            Some(span) => Err(read_after_ignored_error(Some(span))),
        }
    }

    /// Poisons the read at `span` (the first poisoning stays).
    fn poison(&mut self, span: oneyaml_core::Span) {
        self.poisoned.get_or_insert(span);
    }

    /// An error of the event source: the input's own, so the read is
    /// poisoned should a target ignore it.
    fn failed(&mut self, e: Error) -> Error {
        self.poison(e.diagnostic().primary.span);
        e
    }
}

impl<'de, P: Pull<'de>> Pull<'de> for Src<'de, P> {
    fn next(&mut self) -> Result<Ev<'de>, Error> {
        self.check_unpoisoned()?;
        let (ev, planned, prepaid) = match self.queue.pop_front() {
            Some(queued) => queued,
            None => match self.p.next() {
                Ok(ev) => (ev, false, None),
                Err(e) => return Err(self.failed(e)),
            },
        };
        self.count(&ev, 1);
        self.last_end = ev.span.end;
        self.last_planned = planned;
        self.last_prepaid = prepaid;
        Ok(ev)
    }

    fn peek(&mut self) -> Result<&Ev<'de>, Error> {
        self.check_unpoisoned()?;
        if self.queue.is_empty() {
            if let Err(e) = self.p.peek() {
                return Err(self.failed(e));
            }
        }
        match self.queue.front() {
            Some((ev, _, _)) => Ok(ev),
            None => self.p.peek(),
        }
    }

    fn last_end(&self) -> usize {
        self.last_end
    }

    fn peeked_delivered_replay(&self) -> bool {
        // An event pushed back is not the source's.
        self.queue.is_empty() && self.p.peeked_delivered_replay()
    }
}

impl<'de, P> Src<'de, P> {
    /// Pushes back the event just read (it keeps its planned mark).
    fn push_front(&mut self, ev: Ev<'de>) {
        self.count(&ev, -1);
        self.queue.push_front((ev, self.last_planned, None));
    }

    /// Pushes back the event just read for a re-entry of its node that
    /// counted the node's typed level: visiting it as a collection then
    /// costs nothing more (a newtype around a sequence is one level).
    fn push_front_prepaid(&mut self, ev: Ev<'de>, level: usize) {
        let level = level_of(level);
        self.count(&ev, -1);
        self.queue.push_front((ev, self.last_planned, level));
    }

    /// Ends a re-entry at `level`: whether its event is still unread (the
    /// visitor returned without reading the node, which serde allows). The
    /// mark goes either way, so no later reader visits that node free of
    /// charge. Only the front can hold it: reading it is the only way past.
    fn end_prepaid(&mut self, level: usize) -> bool {
        match self.queue.front_mut() {
            Some(front) if front.2.is_some() && front.2 == level_of(level) => {
                front.2 = None;
                true
            }
            _ => false,
        }
    }

    fn push_front_all(&mut self, evs: Vec<Ev<'de>>, planned: bool) {
        self.queue.reserve(evs.len());
        for ev in evs.into_iter().rev() {
            self.count(&ev, -1);
            self.queue.push_front((ev, planned, None));
        }
    }
}

/// The `Y3018` error: a read after an ignored error, located at
/// the node whose error was ignored (`None` for a `Value`, which has no
/// spans).
pub(crate) fn read_after_ignored_error(span: Option<oneyaml_core::Span>) -> Error {
    Error::new(
        oneyaml_core::code::READ_AFTER_IGNORED_ERROR,
        "the deserializer was used after an error it returned for this \
         sequence or mapping was ignored",
        span,
    )
    .with_help(
        "a `Deserialize` impl discarded an error for this sequence or mapping \
         and read on; propagate the error (`?`) instead",
    )
}

/// Whether `e` is the target type's verdict (`Y3000`-`Y3999`, serde),
/// rather than the input's own error (syntax, meaning, limits) or an
/// internal one: a target may ignore the first and read on.
pub(crate) fn is_target_error(e: &Error) -> bool {
    e.code().range() == oneyaml_core::CodeRange::Serde
}

/// A typed level as queue entries mark it (`None`: not representable).
pub(crate) fn level_of(level: usize) -> Option<NonZeroU32> {
    u32::try_from(level).ok().and_then(NonZeroU32::new)
}

/// The state of one deserialization: events, the current document's
/// resolver, and the warnings recorded so far.
pub(crate) struct Core<'de, P> {
    src: Src<'de, P>,
    resolver: Resolver,
    policy: DuplicateKeys,
    warnings: crate::diag_cap::Capped,
    /// Collections currently being visited through the target type's own
    /// `Deserialize` impls (the native-stack recursion).
    typed_depth: usize,
    /// `Limits::max_typed_depth`.
    max_typed_depth: usize,
    /// `Limits::max_diagnostics`.
    max_diagnostics: usize,
    /// Reused allocations for checking skipped content.
    scratch: plan::Scratch<'de>,
    /// Matching `%TAG` tags against variant names, and the
    /// read's byte meter.
    variants: scalar::SharedMemo,
}

impl<'de, P: Pull<'de>> Core<'de, P> {
    /// `Limits::max_diagnostics` of this read.
    pub(crate) const fn max_diagnostics(&self) -> usize {
        self.max_diagnostics
    }

    /// `shared`: the read's byte meter and variant memo, shared with the
    /// expander that `p` reads from.
    pub(crate) fn new(p: P, settings: &Settings, shared: scalar::SharedMemo) -> Self {
        Self {
            src: Src {
                p,
                queue: VecDeque::new(),
                last_end: 0,
                last_planned: false,
                last_prepaid: None,
                open: 0,
                reads: 0,
                poisoned: None,
            },
            resolver: Resolver::new(settings),
            policy: settings.get_duplicate_keys(),
            warnings: crate::diag_cap::Capped::new(settings.get_limits()),
            typed_depth: 0,
            max_typed_depth: settings.get_limits().get_max_typed_depth(),
            max_diagnostics: settings.get_limits().get_max_diagnostics(),
            scratch: plan::Scratch::default(),
            variants: shared,
        }
    }

    /// `Y3018` once a visitor ignored an error that poisons the read and
    /// reads on (`Core::after_error`). Checked where a
    /// target reads (a node, the next element, key, or value) and at the
    /// end of the document, so the outcome is an error with a code, never
    /// an internal error, a misread, or a value for input the `Value` read
    /// refuses; `from_value` reports the same.
    pub(crate) fn check_unpoisoned(&self) -> Result<(), Error> {
        self.src.check_unpoisoned()
    }

    /// Whether the read is poisoned (see `Src::poisoned`).
    pub(crate) fn is_poisoned(&self) -> bool {
        self.src.poisoned.is_some()
    }

    /// Poisons the read at `span` (see `Src::poisoned`).
    pub(crate) fn poison(&mut self, span: oneyaml_core::Span) {
        self.src.poison(span);
    }

    /// Mappings must be buffered and planned (keys.rs rules) when
    /// duplicates are tolerated, and an enum's variant mapping also when
    /// merge keys are on; otherwise they stream (with merge keys on, only
    /// the value of each `<<` is buffered).
    fn needs_plan(&self) -> bool {
        self.resolver.merge_keys() || self.policy == DuplicateKeys::LastWins
    }
}
