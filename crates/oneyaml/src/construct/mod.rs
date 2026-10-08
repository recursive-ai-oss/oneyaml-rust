//! Shared value construction.
//!
//! Everything between "the parser's events" and "a meaning" that is not
//! scalar resolution lives here, **once**, for every consumer:
//!
//! * [`feed`] — the semantic view of the parser's event stream: trivia
//!   skipped, warnings collected, the first `Error` event ends the stream
//!   (R5), `%YAML 1.1` noted per document.
//! * [`expand`] — anchors and aliases: anchored events are recorded on a
//!   flat tape and replayed for each alias, with the incremental alias
//!   budget and `max_depth` of the expanded structure.
//! * [`keys`] — duplicate-key policy and merge-key (`<<`) semantics;
//!   [`merge`] — their O(n log n) accumulation.
//! * [`build`] — an iterative builder of [`Value`](crate::Value)s from
//!   expanded events.
//! * [`scalar`] — resolved scalars as values, and resolver errors as
//!   diagnostics.
//!
//! Consumers pull [`Ev`]ents from a [`Source`]. The serde deserializer reads
//! a [`feed::Feed`] (the parser); the `Document` → `Value` conversion
//! reads a [`tree::TreeSource`] over the syntax tree and reuses [`expand`], [`keys`], and
//! [`build`] unchanged, so both paths give the same values.

use std::borrow::Cow;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use oneyaml_core::Span;
use oneyaml_syntax::event::{TagForm, TagKey, TagSource};
use oneyaml_syntax::tree::Node as TreeNode;

use crate::document::tags::DocTags;
use crate::error::Error;
use crate::resolve::TagInput;
use crate::value::{Tag as ValueTag, TagPrefix, TagStr};

pub(crate) mod budget;
pub(crate) mod build;
pub(crate) mod expand;
pub(crate) mod feed;
pub(crate) mod keys;
pub(crate) mod merge;
pub(crate) mod scalar;
pub(crate) mod settings;
pub(crate) mod tree;

/// A node's tag property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tag<'a> {
    /// No tag.
    Absent,
    /// The non-specific tag `!`.
    NonSpecific,
    /// A specific tag, resolved (`%TAG` expanded).
    Named(TagText<'a>),
}

impl Tag<'_> {
    /// The resolver's view of this tag.
    pub(crate) fn input(&self) -> TagInput<'_> {
        match self {
            Tag::Absent => TagInput::Absent,
            Tag::NonSpecific => TagInput::NonSpecific,
            Tag::Named(t) => TagInput::Named(t.parts()),
        }
    }

    /// The `Value` tag for `name`, which the resolver gave for this tag:
    /// the node's own tag (shared, O(1)); a copy only for a name that is
    /// not this node's (none today).
    pub(crate) fn value_tag(&self, name: TagStr<'_>) -> ValueTag {
        match self {
            Tag::Named(t) => t.value_tag(),
            _ => ValueTag::new(name.to_owned_string()),
        }
    }
}

/// A resolved tag: a [`ValueTag`], which shares its `%TAG`
/// prefix and holds its own suffix, so a use costs O(1) and a distinct tag
/// O(suffix). The lifetime ties it to the read that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TagText<'a>(ValueTag, PhantomData<&'a str>);

impl TagText<'_> {
    pub(crate) fn new(t: ValueTag) -> Self {
        Self(t, PhantomData)
    }

    /// The text, as its pieces (no copy).
    pub(crate) fn parts(&self) -> TagStr<'_> {
        self.0.parts()
    }

    /// The `Value` tag it holds.
    pub(crate) fn tag(&self) -> &ValueTag {
        &self.0
    }

    /// The `Value` tag: the same, shared.
    pub(crate) fn value_tag(&self) -> ValueTag {
        self.0.clone()
    }
}

impl PartialEq<str> for TagText<'_> {
    fn eq(&self, other: &str) -> bool {
        self.0 == *other
    }
}

/// Makes equal tags of one read (or one `Document`) one allocation, so
/// comparing them is O(1): equal **logical** text, however
/// spelled (`%TAG !e! tag:x,1:` + `!e!a`, `!<tag:x,1:a>`, or two directives
/// with the same or overlapping prefixes), gives the same [`ValueTag`].
///
/// An instance seen before is found by its allocation (O(1)). A new one
/// is looked up by its length and digest among the **canonical** tags
/// only (one per text: w.h.p. one per bucket, however many spellings of
/// that text were seen), and verified once, in O(its own text):
/// the gap between two prefixes is checked against the suffix, and
/// whether one prefix starts with another is decided once per pair of
/// prefixes. Prefixes themselves are interned by content, once per
/// directive. Keyed by document content: `RandomState`.
#[derive(Default)]
pub(crate) struct TagInterner {
    prefixes: HashMap<(usize, u64), Vec<Arc<TagPrefix>>>,
    /// Each instance seen, by allocation ([`ValueTag::identity`]; the
    /// instance is kept, so its addresses are not reused), and the
    /// canonical instance of its text.
    seen: HashMap<(usize, usize), (ValueTag, ValueTag)>,
    /// The canonical instances, by (length, digest): one per text.
    canonical: HashMap<(usize, u64), Vec<ValueTag>>,
    /// (shorter prefix, longer prefix) by address: the longer starts with
    /// the shorter.
    starts: HashMap<(usize, usize), bool>,
}

impl TagInterner {
    /// The shared prefix with text `text`. O(text), once per directive.
    pub(crate) fn prefix(&mut self, text: Box<str>) -> Arc<TagPrefix> {
        let p = TagPrefix::new(text);
        let bucket = self
            .prefixes
            .entry((p.text().len(), p.digest()))
            .or_default();
        if let Some(same) = bucket.iter().find(|q| q.text() == p.text()) {
            return Arc::clone(same);
        }
        let p = Arc::new(p);
        bucket.push(Arc::clone(&p));
        p
    }

    /// The canonical instance of `t`. O(1) for an instance seen before;
    /// a new instance is verified, once, against the (w.h.p. at most one)
    /// canonical tag of the same length and digest, and remembered. The
    /// cost never depends on how many spellings of a text were seen
    /// (a bounded cost).
    pub(crate) fn tag(&mut self, t: ValueTag) -> ValueTag {
        let id = t.identity();
        if let Some((_, c)) = self.seen.get(&id) {
            return c.clone();
        }
        let key = (t.len(), t.digest());
        let candidates = self.canonical.get(&key).cloned().unwrap_or_default();
        let mut found = None;
        for c in candidates {
            if self.same_text(&c, &t) {
                found = Some(c);
                break;
            }
        }
        let c = match found {
            Some(c) => c,
            None => {
                self.canonical.entry(key).or_default().push(t.clone());
                t.clone()
            }
        };
        self.seen.insert(id, (t, c.clone()));
        c
    }

    /// Whether `a` and `b` (equal lengths) have the same text: O(the
    /// suffixes), plus once per pair of prefixes O(the shorter prefix).
    fn same_text(&mut self, a: &ValueTag, b: &ValueTag) -> bool {
        if a.same_prefix(b) {
            return a.suffix() == b.suffix();
        }
        let len = |t: &ValueTag| t.shared_prefix().map_or(0, |p| p.text().len());
        let (a, b) = if len(a) <= len(b) { (a, b) } else { (b, a) };
        let addr = |t: &ValueTag| t.shared_prefix().map_or(0, |p| Arc::as_ptr(p).addr());
        let pre_a = a.shared_prefix().map_or("", |p| p.text());
        let pre_b = b.shared_prefix().map_or("", |p| p.text());
        let starts = *self
            .starts
            .entry((addr(a), addr(b)))
            .or_insert_with(|| pre_b.starts_with(pre_a));
        // `pre_b` = `pre_a` + gap: `a`'s suffix must be gap + `b`'s suffix.
        starts
            && a.suffix()
                .strip_prefix(pre_b.get(pre_a.len()..).unwrap_or(""))
                .is_some_and(|rest| rest == b.suffix())
    }
}

/// Resolves tags for one read: each distinct tag
/// once, in O(its suffix), sharing each `%TAG` prefix, decoded once per
/// directive. Memory: O(the distinct suffixes) plus one copy of each
/// prefix, so no bound or eviction is needed. A read of a `Document`'s
/// nodes asks the document's table ([`DocTags`]) instead.
#[derive(Default)]
pub(crate) struct TagCache<'a> {
    seen: HashMap<TagKey<'a>, ValueTag>,
    /// Each directive's decoded prefix: the shared whole characters, and
    /// the bytes of a character its tags' suffixes complete.
    directives: HashMap<TagKey<'a>, (Arc<TagPrefix>, Box<[u8]>)>,
    interner: TagInterner,
    doc: Option<&'a DocTags>,
}

impl<'a> TagCache<'a> {
    /// A cache that asks `doc`, the document's table (node reads).
    pub(crate) fn with_doc(doc: &'a DocTags) -> Self {
        Self {
            doc: Some(doc),
            ..Self::default()
        }
    }

    /// The tag of tree node `n`: a `%TAG`-prefixed tag from the document's
    /// table when there is one, else as [`TagCache::tag`].
    ///
    /// # Errors
    ///
    /// `Y9000` as [`TagCache::tag`].
    pub(crate) fn node(&mut self, n: TreeNode<'a>) -> Result<Tag<'a>, Error> {
        let source = n.tag_source();
        match self.doc {
            Some(doc) if source.as_ref().is_some_and(|t| t.prefix_key().is_some()) => {
                Ok(match doc.try_tag(n)? {
                    // One instance per text in this read too (the table's
                    // and this read's own, e.g. a verbatim spelling):
                    // O(1) per use once seen.
                    Tag::Named(t) => Tag::Named(TagText::new(self.interner.tag(t.value_tag()))),
                    other => other,
                })
            }
            _ => self.tag(source),
        }
    }

    /// The node tag of `t` (`None`: no tag). O(the property's text).
    ///
    /// # Errors
    ///
    /// `Y9000` (internal) for a property whose parts do not join to UTF-8,
    /// which the parser rules out.
    pub(crate) fn tag(&mut self, t: Option<TagSource<'a>>) -> Result<Tag<'a>, Error> {
        let Some(t) = t else { return Ok(Tag::Absent) };
        if t.form == TagForm::NonSpecific {
            return Ok(Tag::NonSpecific);
        }
        let key = t.key();
        if let Some(v) = self.seen.get(&key) {
            return Ok(Tag::Named(TagText::new(v.clone())));
        }
        let prefix = match t.prefix_key() {
            Some(pk) => {
                let interner = &mut self.interner;
                let entry = self.directives.entry(pk).or_insert_with(|| {
                    let bytes = t.directive_prefix().unwrap_or_default();
                    split_prefix(interner, &bytes)
                });
                Some((Arc::clone(&entry.0), &*entry.1))
            }
            None => None,
        };
        let Some(v) = build_tag(&t, prefix) else {
            // The parser validated the property (its parts join to UTF-8).
            // Never a panic, and never a silent reading as untagged:
            // an internal error in release.
            debug_assert!(false, "a tag property that does not resolve");
            return Err(Error::internal(
                "a tag property that does not resolve",
                Some(t.span),
            ));
        };
        let v = self.interner.tag(v);
        self.seen.insert(key, v.clone());
        Ok(Tag::Named(TagText::new(v)))
    }

    /// The node tag of `t` on its own (one node's read, a tag the
    /// document's table does not know). O(prefix + suffix).
    ///
    /// # Errors
    ///
    /// `Y9000` as [`TagCache::tag`].
    pub(crate) fn one(t: Option<TagSource<'a>>) -> Result<Tag<'a>, Error> {
        Self::default().tag(t)
    }
}

/// A directive's decoded prefix split at its last whole character: the
/// shared text, and the bytes of a character its tags' suffixes complete.
fn split_prefix(interner: &mut TagInterner, bytes: &[u8]) -> (Arc<TagPrefix>, Box<[u8]>) {
    let valid = match core::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) => e.valid_up_to(),
    };
    let (head, tail) = bytes.split_at(valid.min(bytes.len()));
    let text = core::str::from_utf8(head).unwrap_or_default();
    (interner.prefix(text.into()), tail.into())
}

/// The tag of `t`: `prefix` (its directive's shared prefix and carried
/// bytes) or its default prefix, then its decoded suffix. O(suffix).
/// `None` when the parts do not join to UTF-8 (the parser refuses those).
fn build_tag(t: &TagSource<'_>, prefix: Option<(Arc<TagPrefix>, &[u8])>) -> Option<ValueTag> {
    let suffix = t.decoded_suffix();
    let (shared, lead): (Option<Arc<TagPrefix>>, &[u8]) = match prefix {
        Some((p, carried)) => (Some(p), carried),
        None => (None, t.default_prefix().unwrap_or_default().as_bytes()),
    };
    let text = if lead.is_empty() {
        match suffix {
            Cow::Borrowed(b) => Arc::from(core::str::from_utf8(b).ok()?),
            Cow::Owned(b) => Arc::from(String::from_utf8(b).ok()?),
        }
    } else {
        let mut joined = Vec::with_capacity(lead.len().saturating_add(suffix.len()));
        joined.extend_from_slice(lead);
        joined.extend_from_slice(&suffix);
        Arc::from(String::from_utf8(joined).ok()?)
    };
    Some(ValueTag::from_parts(shared, text))
}

/// One semantic event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node<'a> {
    /// Start of a document; `yaml11`: it carries `%YAML 1.1` (or 1.0).
    DocStart {
        yaml11: bool,
    },
    DocEnd,
    /// A scalar: its value, whether it was plain, and its tag.
    Scalar {
        value: Cow<'a, str>,
        plain: bool,
        tag: Tag<'a>,
    },
    SeqStart {
        tag: Tag<'a>,
    },
    SeqEnd,
    MapStart {
        tag: Tag<'a>,
    },
    MapEnd,
    /// `*name`. Never leaves [`expand::Expander`].
    Alias {
        name: &'a str,
    },
    /// End of the stream (repeated forever once reached).
    StreamEnd,
}

/// A semantic event with its anchor and span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ev<'a> {
    pub(crate) node: Node<'a>,
    pub(crate) anchor: Option<&'a str>,
    pub(crate) span: Span,
    /// The event stands at a source position that was already read (so its
    /// warnings are not reported twice): an alias replay's events inside the
    /// anchored node, and the edges of a replay whose alias was itself
    /// replayed. Meaning warnings at such a position (a tolerated duplicate
    /// key) were recorded when the source was read, so they are not repeated:
    /// each source duplicate is reported once, as `Document` reports it.
    pub(crate) replayed: bool,
}

impl<'a> Ev<'a> {
    pub(crate) fn new(node: Node<'a>, span: Span) -> Self {
        Self {
            node,
            anchor: None,
            span,
            replayed: false,
        }
    }

    /// True for the events that start a node.
    pub(crate) fn starts_node(&self) -> bool {
        matches!(
            self.node,
            Node::Scalar { .. }
                | Node::SeqStart { .. }
                | Node::MapStart { .. }
                | Node::Alias { .. }
        )
    }
}

/// A source of semantic events: the parser ([`feed::Feed`]) today, the
/// syntax tree later.
pub(crate) trait Source<'a> {
    /// The next event. After `StreamEnd`, `StreamEnd` again. An `Err` ends
    /// the stream: later calls return `StreamEnd` or the same kind of error.
    fn next_event(&mut self) -> Result<Ev<'a>, Error>;

    /// Warnings recorded so far (moved out), with how many more were
    /// counted but not kept.
    fn take_warnings(&mut self) -> (Vec<oneyaml_core::Diagnostic>, crate::diag_cap::Dropped);

    /// Where warnings go from now on (kept under the cap, or at
    /// most 16 on paths that discard them; the rest counted).
    fn collect_warnings(&mut self, warnings: crate::diag_cap::Capped);

    /// The node whose event [`Source::next_event`] returned last, to read
    /// again, for sources that can.
    fn replay_last(&self) -> Option<tree::Replay<'a>> {
        None
    }

    /// Anchored nodes from outside what this source reads that its aliases
    /// refer to (a per-node read, [`tree::NodeSource`]), in document order:
    /// [`expand::Expander`] records them right after the next `DocStart`
    /// and never delivers them, so they are what an alias expands to, not
    /// content (replaying them counts against the alias budget).
    fn take_preload(&mut self) -> Result<Vec<Ev<'a>>, Error> {
        Ok(Vec::new())
    }

    /// For a source that reads part of a document (a per-node read): how
    /// many own nodes of the document precede what it reads, so the alias
    /// budget is the one the whole read has at the same point.
    /// `None`: the source reads whole documents.
    fn nodes_before(&mut self) -> Option<usize> {
        None
    }

    /// For a source that reads part of a document (a per-node read): where
    /// the document starts (its `DocStart` event's start in a whole read),
    /// so the alias byte budget, which scales with the bytes of the
    /// document read so far, is the whole read's at the same place.
    /// `None`: the source's own `DocStart` events say.
    fn document_start(&mut self) -> Option<usize> {
        None
    }
}

/// A consumer's view of expanded events (no aliases, no anchors): the
/// [`expand::Expander`] over a source, or a [`Recording`] over another pull.
pub(crate) trait Pull<'a> {
    /// The next event.
    fn next(&mut self) -> Result<Ev<'a>, Error>;
    /// The next event, without consuming it.
    fn peek(&mut self) -> Result<&Ev<'a>, Error>;
    /// The end offset of the last event returned by [`Pull::next`].
    fn last_end(&self) -> usize;
    /// Whether the event just peeked starts an alias's replay of a node
    /// this read delivered (and so checked) where it was defined; `false`
    /// when that is not known.
    fn peeked_delivered_replay(&self) -> bool {
        false
    }
}

impl<'a, S: Source<'a>> Pull<'a> for expand::Expander<'a, S> {
    fn next(&mut self) -> Result<Ev<'a>, Error> {
        expand::Expander::next(self)
    }
    fn peek(&mut self) -> Result<&Ev<'a>, Error> {
        expand::Expander::peek(self)
    }
    fn last_end(&self) -> usize {
        expand::Expander::last_end(self)
    }
    fn peeked_delivered_replay(&self) -> bool {
        self.starts_delivered_replay()
    }
}

/// A [`Pull`] that keeps a copy of every event it hands out, so a node can
/// be built (and checked) while it is read, then replayed. Building
/// as events arrive meets each error in the same order as the `Value`
/// builder, where buffering the whole node first would report a later
/// syntax error ahead of an earlier meaning error.
pub(crate) struct Recording<'p, 'a, P> {
    inner: &'p mut P,
    seen: Vec<Ev<'a>>,
}

impl<'p, 'a, P: Pull<'a>> Recording<'p, 'a, P> {
    /// Records from `inner`, starting with `first` (already read).
    pub(crate) fn new(inner: &'p mut P, first: &Ev<'a>) -> Self {
        Self {
            inner,
            seen: vec![first.clone()],
        }
    }

    /// The events handed out, in order.
    pub(crate) fn into_events(self) -> Vec<Ev<'a>> {
        self.seen
    }
}

impl<'a, P: Pull<'a>> Pull<'a> for Recording<'_, 'a, P> {
    fn next(&mut self) -> Result<Ev<'a>, Error> {
        let ev = self.inner.next()?;
        self.seen.push(ev.clone());
        Ok(ev)
    }
    fn peek(&mut self) -> Result<&Ev<'a>, Error> {
        self.inner.peek()
    }
    fn last_end(&self) -> usize {
        self.inner.last_end()
    }
}
