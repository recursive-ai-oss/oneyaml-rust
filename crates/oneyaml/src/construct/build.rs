//! Iterative construction of [`Value`]s from expanded events (an
//! explicit frame stack; depth is already bounded by the [`Pull`] source).

use oneyaml_core::{DuplicateKeys, Span};

use super::keys::{Merge, duplicate_at, invalid_merge, key_text};
use super::merge::MergeAcc;
use super::scalar::{is_merge_key, resolve, resolve_collection, scalar_value};
use super::{Ev, Node, Pull, Tag as NodeTag};
use crate::error::Error;
use crate::resolve::{CollectionKind, ResolvedCollection, Resolver};
use crate::value::{Mapping, Sequence, Tag, TaggedValue, Value};

/// What construction needs besides events: the document's resolver, the
/// duplicate-key policy, and where warnings go.
pub(crate) struct Cx<'w> {
    pub(crate) resolver: Resolver,
    pub(crate) policy: DuplicateKeys,
    pub(crate) warnings: &'w mut crate::diag_cap::Capped,
}

#[cfg(test)]
thread_local! {
    /// Events read by the builder on this thread (the scaling tests).
    pub(crate) static BUILT_EVENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A mapping's entries as a merge source.
type Source = MergeAcc<Value, Value>;

/// A key read, with its span and whether that span is a replayed
/// position ([`Ev::replayed`]).
enum PendingKey {
    Key(Value, Span, bool),
    Merge(Span, bool),
}

struct MapFrame {
    map: Mapping,
    /// Span of each entry's first key, by position.
    spans: Vec<Span>,
    key: Option<PendingKey>,
    merge: Merge<Value, Value>,
    /// The value of a `<<` (or an item of one): delivered as its entries,
    /// never materialised as a `Mapping`.
    source: bool,
}

/// A sequence that is the value of a `<<`: its items are merge sources.
struct SeqMerge {
    /// The items' entries so far (boxed: frames stay small).
    acc: Option<Box<Source>>,
    all_maps: bool,
    len: usize,
}

enum Frame {
    Seq {
        items: Vec<Value>,
        custom: Option<Tag>,
        span: Span,
        /// The start event's [`Ev::replayed`].
        replayed: bool,
        merge: Option<SeqMerge>,
    },
    Map {
        m: Box<MapFrame>,
        custom: Option<Tag>,
        span: Span,
        /// The start event's [`Ev::replayed`].
        replayed: bool,
    },
}

/// A completed node.
enum Built {
    Value(Value),
    /// A merge-source mapping's entries.
    Source(Box<Source>),
    /// A `<<` sequence: its mapping items' entries, and whether every item
    /// was a mapping.
    Sources(Option<Box<Source>>, bool),
}

fn custom_of(r: ResolvedCollection<'_>, tag: &NodeTag<'_>) -> Option<Tag> {
    match r {
        ResolvedCollection::Plain => None,
        ResolvedCollection::Custom(t) => Some(tag.value_tag(t)),
    }
}

fn wrap(custom: Option<Tag>, value: Value) -> Value {
    match custom {
        None => value,
        Some(tag) => Value::Tagged(Box::new(TaggedValue { tag, value })),
    }
}

/// Whether the next node of `stack`'s top is (part of) a merge value.
fn in_merge(stack: &[Frame]) -> bool {
    match stack.last() {
        Some(Frame::Map { m, .. }) => matches!(m.key, Some(PendingKey::Merge(..))),
        Some(Frame::Seq { merge, .. }) => merge.is_some(),
        None => false,
    }
}

impl MapFrame {
    fn deliver(
        &mut self,
        b: Built,
        span: Span,
        replayed: bool,
        cx: &mut Cx<'_>,
    ) -> Result<(), Error> {
        match self.key.take() {
            Some(PendingKey::Merge(at, at_replayed)) => {
                // As before: a repeated `<<` first, then the shape.
                self.merge.key(at, at_replayed, cx.policy, cx.warnings)?;
                match b {
                    Built::Source(acc) | Built::Sources(Some(acc), true) => self.merge.absorb(acc),
                    Built::Sources(None, true) => {}
                    _ => return Err(invalid_merge(at)),
                }
            }
            None => match b {
                Built::Value(v) => self.key = Some(PendingKey::Key(v, span, replayed)),
                _ => return Err(Error::internal("merge source as a key", Some(span))),
            },
            Some(PendingKey::Key(k, at, at_replayed)) => {
                let Built::Value(v) = b else {
                    return Err(Error::internal("merge source as a value", Some(span)));
                };
                let (i, old) = self.map.insert_full(k, v);
                if old.is_some() {
                    // Same rule as the serde path: first position, last
                    // value (keys.rs).
                    let first = self.spans.get(i).copied().unwrap_or(at);
                    let map = &self.map;
                    let text = || {
                        map.get_index(i)
                            .map(|(k, _)| key_text(k))
                            .unwrap_or_default()
                    };
                    duplicate_at(cx.warnings, cx.policy, at_replayed, text, first, at)?;
                } else {
                    self.spans.push(at);
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Built, Error> {
        let MapFrame {
            map,
            key,
            merge,
            source,
            ..
        } = self;
        if key.is_some() {
            return Err(Error::internal("mapping ended after a key", None));
        }
        if source {
            let explicit = MergeAcc::from_entries(map);
            let all = if merge.is_empty() {
                explicit
            } else {
                merge.finish(explicit)
            };
            return Ok(Built::Source(Box::new(all)));
        }
        // Materialised once: the explicit entries as built, then
        // the merged ones they do not have.
        let mut map = map;
        if let Some(merged) = merge.into_merged() {
            for (k, v) in merged.into_entries() {
                if !map.contains_key(&k) {
                    map.insert(k, v);
                }
            }
        }
        Ok(Built::Value(Value::Mapping(map)))
    }
}

/// Builds the node whose first event is `first`, reading the rest from `p`.
pub(crate) fn build_from<'a>(
    p: &mut impl Pull<'a>,
    cx: &mut Cx<'_>,
    first: Ev<'a>,
) -> Result<Value, Error> {
    let mut stack: Vec<Frame> = Vec::new();
    run(p, cx, first, &mut stack).map_err(|e| with_path(e, &stack))
}

/// Prepends the path of the open frames (innermost first) to an error.
fn with_path(mut e: Error, stack: &[Frame]) -> Error {
    for f in stack.iter().rev() {
        e = match f {
            Frame::Seq { items, merge, .. } => {
                e.in_index(merge.as_ref().map_or(items.len(), |m| m.len))
            }
            Frame::Map { m, .. } => match &m.key {
                Some(PendingKey::Key(k, ..)) => e.in_key(&key_text(k)),
                Some(PendingKey::Merge(..)) => e.in_key("<<"),
                None => e,
            },
        };
    }
    e
}

fn run<'a>(
    p: &mut impl Pull<'a>,
    cx: &mut Cx<'_>,
    first: Ev<'a>,
    stack: &mut Vec<Frame>,
) -> Result<Value, Error> {
    let mut ev = first;
    loop {
        #[cfg(test)]
        BUILT_EVENTS.with(|c| c.set(c.get().saturating_add(1)));
        let span = ev.span;
        let replayed = ev.replayed;
        let done = match ev.node {
            Node::Scalar { value, plain, tag } => {
                let merge_slot = match stack.last_mut() {
                    Some(Frame::Map { m, .. })
                        if m.key.is_none() && is_merge_key(&cx.resolver, &tag, plain, &value) =>
                    {
                        m.key = Some(PendingKey::Merge(span, replayed));
                        true
                    }
                    _ => false,
                };
                if merge_slot {
                    None
                } else {
                    let r = resolve(&cx.resolver, &tag, plain, &value, span)?;
                    Some((Built::Value(scalar_value(r, value, &tag)), span, replayed))
                }
            }
            Node::SeqStart { tag } => {
                let r = resolve_collection(&cx.resolver, &tag, CollectionKind::Sequence, span)?;
                let custom = custom_of(r, &tag);
                // The items of a `<<` sequence are merge sources.
                let merge_value = matches!(
                    stack.last(),
                    Some(Frame::Map { m, .. }) if matches!(m.key, Some(PendingKey::Merge(..)))
                );
                let merge = (merge_value && custom.is_none()).then(|| SeqMerge {
                    acc: None,
                    all_maps: true,
                    len: 0,
                });
                stack.push(Frame::Seq {
                    items: Vec::new(),
                    custom,
                    span,
                    replayed,
                    merge,
                });
                None
            }
            Node::MapStart { tag } => {
                let r = resolve_collection(&cx.resolver, &tag, CollectionKind::Mapping, span)?;
                let custom = custom_of(r, &tag);
                let source = custom.is_none() && in_merge(stack);
                stack.push(Frame::Map {
                    m: Box::new(MapFrame {
                        map: Mapping::new(),
                        spans: Vec::new(),
                        key: None,
                        merge: Merge::default(),
                        source,
                    }),
                    custom,
                    span,
                    replayed,
                });
                None
            }
            Node::SeqEnd => match stack.pop() {
                Some(Frame::Seq {
                    merge: Some(m),
                    span,
                    replayed,
                    ..
                }) => Some((Built::Sources(m.acc, m.all_maps), span, replayed)),
                Some(Frame::Seq {
                    items,
                    custom,
                    span,
                    replayed,
                    merge: None,
                }) => Some((
                    Built::Value(wrap(custom, Value::Sequence(Sequence::from(items)))),
                    span,
                    replayed,
                )),
                _ => return Err(Error::internal("unbalanced sequence end", Some(span))),
            },
            Node::MapEnd => match stack.pop() {
                Some(Frame::Map {
                    m,
                    custom,
                    span,
                    replayed,
                }) => {
                    let built = match (*m).finish().map_err(|e| e.at(span))? {
                        Built::Value(v) => Built::Value(wrap(custom, v)),
                        other => other,
                    };
                    Some((built, span, replayed))
                }
                _ => return Err(Error::internal("unbalanced mapping end", Some(span))),
            },
            _ => return Err(Error::internal("node expected", Some(span))),
        };
        // `node_span`: a scalar's own span, or a collection's start (for
        // "first defined here").
        if let Some((b, node_span, node_replayed)) = done {
            match stack.last_mut() {
                None => {
                    return match b {
                        Built::Value(v) => Ok(v),
                        _ => Err(Error::internal("merge source at the root", Some(span))),
                    };
                }
                Some(Frame::Seq { merge: Some(m), .. }) => {
                    m.len = m.len.saturating_add(1);
                    match b {
                        Built::Source(src) => match m.acc.as_mut() {
                            None => m.acc = Some(src),
                            Some(acc) => acc.absorb_later(*src),
                        },
                        _ => m.all_maps = false,
                    }
                }
                Some(Frame::Seq { items, .. }) => match b {
                    Built::Value(v) => items.push(v),
                    _ => return Err(Error::internal("merge source as an item", Some(span))),
                },
                Some(Frame::Map { m, .. }) => m.deliver(b, node_span, node_replayed, cx)?,
            }
        }
        ev = p.next()?;
    }
}
