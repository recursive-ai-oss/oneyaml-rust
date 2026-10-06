//! `SeqAccess`, `MapAccess`, and enum access over [`Core`].
//!
//! Paths are attached while an error unwinds (`in_index` / `in_key` on the
//! `Err` branch only), so the happy path tracks nothing.

use std::borrow::Cow;

use serde::de::value::StrDeserializer;
use serde::de::{self, DeserializeSeed, IntoDeserializer, Visitor};

use super::scalar::{Sc, ScalarDe};
use super::{Core, Hint};
use crate::construct::build::{Cx, build_from};
use crate::construct::keys::{KeyRepr, KeySet, duplicate_at, key_text, on_duplicate};
use crate::construct::{Node, Pull, Recording};
use crate::error::Error;

pub(crate) struct SeqAccess<'c, 'de, P> {
    core: &'c mut Core<'de, P>,
    index: usize,
    done: bool,
}

impl<'c, 'de, P: Pull<'de>> SeqAccess<'c, 'de, P> {
    pub(crate) fn new(core: &'c mut Core<'de, P>) -> Self {
        Self {
            core,
            index: 0,
            done: false,
        }
    }

    /// (reached the end, elements taken).
    pub(crate) fn finish(&self) -> (bool, usize) {
        (self.done, self.index)
    }
}

impl<'de, P: Pull<'de>> de::SeqAccess<'de> for SeqAccess<'_, 'de, P> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.done {
            return Ok(None);
        }
        self.core.check_unpoisoned()?;
        if matches!(self.core.src.peek()?.node, Node::SeqEnd) {
            self.core.src.next()?;
            self.done = true;
            return Ok(None);
        }
        let i = self.index;
        self.index = i.saturating_add(1);
        seed.deserialize(&mut *self.core)
            .map(Some)
            .map_err(|e| e.in_index(i))
    }
}

/// The current key, for paths.
enum KeyName<'de> {
    Text(Cow<'de, str>),
    Complex(String),
}

pub(crate) struct MapAccess<'c, 'de, P> {
    core: &'c mut Core<'de, P>,
    keys: KeySet<'de>,
    key: Option<KeyName<'de>>,
    /// The current key's identity and span (and whether that span is a
    /// replayed position, [`Ev::replayed`](crate::construct::Ev)), checked
    /// for duplicates once its value is read: the shared builder's order
    /// (both readers agree), so the value's own errors come first.
    pending: Option<(KeyRepr<'de>, oneyaml_core::Span, bool)>,
    /// The current key was read and its value was not (what the
    /// rest of a failed read starts with).
    value_due: bool,
    done: bool,
}

impl<'c, 'de, P: Pull<'de>> MapAccess<'c, 'de, P> {
    pub(crate) fn new(core: &'c mut Core<'de, P>) -> Self {
        Self {
            core,
            keys: KeySet::default(),
            key: None,
            pending: None,
            value_due: false,
            done: false,
        }
    }

    /// Whether the read is not poisoned (only then is the rest
    /// of the mapping where the stream stands).
    pub(crate) fn core_unpoisoned(&self) -> bool {
        !self.core.is_poisoned()
    }

    /// Reads what the visitor left of the mapping, its end included, as
    /// the `Value` builder would ([`Core::finish_mapping`]): the current
    /// entry's value if it is due, its duplicate check, then the other
    /// entries, checked against every key read before.
    pub(crate) fn finish_rest(&mut self) -> Result<(), Error> {
        if self.done {
            return Ok(());
        }
        if self.value_due {
            self.value_due = false;
            let text = self.key_text().to_owned();
            self.core.skip_node().map_err(|e| e.in_key(&text))?;
        }
        self.duplicate()?;
        self.done = true;
        self.core.finish_mapping(&mut self.keys).map(|_| ())
    }

    /// The current key's text, for paths and messages.
    fn key_text(&self) -> &str {
        match &self.key {
            Some(KeyName::Text(t)) => t.as_ref(),
            Some(KeyName::Complex(t)) => t.as_str(),
            None => "",
        }
    }

    /// A key equal to an earlier one, under a policy that refuses it: the
    /// input's meaning error (Y1013), reported before the target sees the
    /// key, so a target's own verdict on a repeated key (serde's derived
    /// "duplicate field", a variant mapping's "invalid length") never
    /// replaces it (the `Value` read reports Y1013 here). The value is
    /// still read first, validated like `IgnoredAny`: the shared builder's
    /// order, so the value's own errors come first.
    fn refuse_duplicate(&mut self) -> Result<(), Error> {
        let Some((repr, span, _)) = &self.pending else {
            return Ok(());
        };
        let Some(first) = self.keys.first(repr) else {
            return Ok(());
        };
        let text = match &self.key {
            Some(KeyName::Text(t)) => t.as_ref(),
            Some(KeyName::Complex(t)) => t.as_str(),
            None => "",
        };
        // A tolerated duplicate is recorded once its value is read.
        let Err(dup) = on_duplicate(self.core.policy, text, first, *span) else {
            return Ok(());
        };
        let text = text.to_owned();
        self.value_due = false;
        self.pending = None;
        self.core.skip_node().map_err(|e| e.in_key(&text))?;
        Err(dup)
    }

    /// The duplicate check of the entry whose value was just read.
    fn duplicate(&mut self) -> Result<(), Error> {
        let Some((repr, span, replayed)) = self.pending.take() else {
            return Ok(());
        };
        let text = match &self.key {
            Some(KeyName::Text(t)) => t.as_ref(),
            Some(KeyName::Complex(t)) => t.as_str(),
            None => "",
        };
        if let Some(first) = self.keys.insert(repr, span) {
            let policy = self.core.policy;
            duplicate_at(
                &mut self.core.warnings,
                policy,
                replayed,
                || text.to_owned(),
                first,
                span,
            )?;
        }
        Ok(())
    }
}

impl<'de, P: Pull<'de>> de::MapAccess<'de> for MapAccess<'_, 'de, P> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.done {
            return Ok(None);
        }
        self.core.check_unpoisoned()?;
        let span = self.core.src.peek()?.span;
        let depth = self.core.src.open;
        // An error here that the target may not ignore (the
        // input's own, `Core::after_error`) poisons the read.
        let result = self.settle().and_then(|()| self.next_key_unchecked(seed));
        if let Err(e) = &result {
            self.core.after_error(e, span, depth);
        }
        result
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        self.core.check_unpoisoned()?;
        let span = self.core.src.peek()?.span;
        let depth = self.core.src.open;
        let result = self.next_value_unchecked(seed);
        if let Err(e) = &result {
            self.core.after_error(e, span, depth);
        }
        result
    }
}

impl<'de, P: Pull<'de>> MapAccess<'_, 'de, P> {
    /// The entry before, if the target ignored an error and asks
    /// for the next key: its value is skipped (checked as ignored content)
    /// if it is still due, and its key checked for duplicates, so the read
    /// stays in step, as `from_value` reads on.
    fn settle(&mut self) -> Result<(), Error> {
        if self.value_due {
            self.value_due = false;
            let text = self.key_text().to_owned();
            self.core.skip_node().map_err(|e| e.in_key(&text))?;
        }
        self.duplicate()
    }

    fn next_key_unchecked<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let ev = self.core.src.next()?;
        let (span, replayed) = (ev.span, ev.replayed);
        if !matches!(ev.node, Node::MapEnd) {
            self.value_due = true;
        }
        match ev.node {
            Node::MapEnd => {
                self.done = true;
                Ok(None)
            }
            Node::Scalar { value, plain, tag } => {
                let sc = self.core.with_memo(
                    Sc::classify(&self.core.resolver, tag, plain, value, span)?
                        .typed(self.core.typed_depth, self.core.max_typed_depth),
                );
                let repr = scalar_key(&sc);
                self.pending = Some((repr, span, replayed));
                self.key = Some(KeyName::Text(sc.value.clone()));
                self.refuse_duplicate()?;
                seed.deserialize(ScalarDe(sc))
                    .map(Some)
                    .map_err(|e| e.at(span))
            }
            Node::SeqStart { .. } | Node::MapStart { .. } => {
                // A complex key: build its `Value` for the duplicate check
                // while recording its events, then replay them to the
                // target. Built as read, so its errors come in the
                // `Value` builder's order (not after a later syntax error).
                let planned = self.core.src.last_planned;
                let (key, events) = {
                    let mut rec = Recording::new(&mut self.core.src, &ev);
                    let mut cx = Cx {
                        resolver: self.core.resolver,
                        policy: self.core.policy,
                        warnings: &mut self.core.warnings,
                    };
                    let key = build_from(&mut rec, &mut cx, ev)?;
                    (key, rec.into_events())
                };
                let text = key_text(&key);
                self.pending = Some((KeyRepr::of_value(key), span, replayed));
                self.key = Some(KeyName::Complex(text));
                // The key's events are not replayed to a refused entry.
                self.refuse_duplicate()?;
                self.core.src.push_front_all(events, planned);
                seed.deserialize(&mut *self.core)
                    .map(Some)
                    .map_err(|e| e.at(span))
            }
            _ => Err(Error::internal("mapping key expected", Some(span))),
        }
    }

    fn next_value_unchecked<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Error> {
        // Read now: on an error, `node` consumed (or poisoned) it.
        self.value_due = false;
        let value = seed
            .deserialize(&mut *self.core)
            .map_err(|e| match &self.key {
                Some(KeyName::Text(t)) => e.in_key(t),
                Some(KeyName::Complex(t)) => e.in_key(t),
                None => e,
            })?;
        self.duplicate()?;
        Ok(value)
    }
}

/// A scalar key's identity for the duplicate check (the builder's rules).
pub(crate) fn scalar_key<'de>(sc: &Sc<'de>) -> KeyRepr<'de> {
    match sc.kind {
        super::scalar::Kind::Str => KeyRepr::Str(sc.value.clone()),
        _ => KeyRepr::of_value(sc.to_value()),
    }
}

/// What a `{Variant: content}` read left for the rest of the mapping
/// (`Core::visit_map_enum`).
#[derive(Default)]
pub(crate) struct EnumState<'de> {
    /// The variant key's identity and span, once it is read.
    pub(crate) key: Option<(KeyRepr<'de>, oneyaml_core::Span)>,
    /// The variant key was refused (its content, or the key itself, is
    /// unread).
    pub(crate) refused: bool,
    /// The content is unread: the variant key's text, for paths.
    pub(crate) content_due: Option<String>,
}

/// `{Variant: content}`: an externally tagged enum.
pub(crate) struct MapEnum<'c, 'de, P> {
    core: &'c mut Core<'de, P>,
    state: &'c mut EnumState<'de>,
}

impl<'c, 'de, P: Pull<'de>> MapEnum<'c, 'de, P> {
    /// `state` receives what the read leaves.
    pub(crate) fn new(core: &'c mut Core<'de, P>, state: &'c mut EnumState<'de>) -> Self {
        Self { core, state }
    }
}

impl<'c, 'de, P: Pull<'de>> de::EnumAccess<'de> for MapEnum<'c, 'de, P> {
    type Error = Error;
    type Variant = Variant<'c, 'de, P>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Error> {
        let ev = self.core.src.next()?;
        let span = ev.span;
        match ev.node {
            Node::Scalar { value, plain, tag } => {
                let sc = match Sc::classify(&self.core.resolver, tag, plain, value, span) {
                    Ok(sc) => sc,
                    Err(e) => {
                        // The input's error; ignoring it poisons.
                        let depth = self.core.src.open;
                        self.core.after_error(&e, span, depth);
                        return Err(e);
                    }
                };
                let sc = self
                    .core
                    .with_memo(sc.typed(self.core.typed_depth, self.core.max_typed_depth));
                let name: Cow<'de, str> = sc.value.clone();
                self.state.key = Some((scalar_key(&sc), span));
                self.state.content_due = Some(name.clone().into_owned());
                let variant = match seed.deserialize(ScalarDe(sc)) {
                    Ok(v) => v,
                    Err(e) => {
                        // The rest is read by `Core::visit_map_enum`.
                        self.state.refused = true;
                        return Err(e.at(span));
                    }
                };
                Ok((
                    variant,
                    Variant {
                        core: self.core,
                        name: Cow::Owned(name.into_owned()),
                        due: Some(&mut self.state.content_due),
                    },
                ))
            }
            Node::MapEnd => Err(de::Error::invalid_length(
                0,
                &"a mapping with a single key (the variant)",
            )),
            _ => {
                // The key node is part of the rest `Core::visit_map_enum`
                // reads.
                self.core.src.push_front(ev);
                self.state.refused = true;
                Err(de::Error::invalid_type(
                    de::Unexpected::Other("a collection as variant name"),
                    &"a variant name",
                ))
            }
        }
    }
}

/// `!Variant content` on a collection.
pub(crate) struct TaggedCollection<'c, 'de, P> {
    core: &'c mut Core<'de, P>,
    /// The variant name (a `'static` name when it matched).
    name: Cow<'static, str>,
}

impl<'c, 'de, P: Pull<'de>> TaggedCollection<'c, 'de, P> {
    pub(crate) fn new(core: &'c mut Core<'de, P>, name: Cow<'static, str>) -> Self {
        Self { core, name }
    }
}

impl<'c, 'de, P: Pull<'de>> de::EnumAccess<'de> for TaggedCollection<'c, 'de, P> {
    type Error = Error;
    type Variant = Variant<'c, 'de, P>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Error> {
        let de: StrDeserializer<'_, Error> = (&*self.name).into_deserializer();
        let variant = seed.deserialize(de)?;
        Ok((
            variant,
            Variant {
                core: self.core,
                name: self.name,
                due: None,
            },
        ))
    }
}

/// The content of a variant, read from the stream.
pub(crate) struct Variant<'c, 'de, P> {
    core: &'c mut Core<'de, P>,
    name: Cow<'static, str>,
    /// `EnumState::content_due` of a `{Variant: content}` read, cleared
    /// once the content is read.
    due: Option<&'c mut Option<String>>,
}

impl<P> Variant<'_, '_, P> {
    /// The content is being read: on an error, `node` consumed it (or
    /// poisoned the read).
    fn reading(&mut self) {
        if let Some(due) = self.due.as_deref_mut() {
            *due = None;
        }
    }
}

impl<'de, P: Pull<'de>> de::VariantAccess<'de> for Variant<'_, 'de, P> {
    type Error = Error;

    fn unit_variant(mut self) -> Result<(), Error> {
        self.reading();
        let name = self.name;
        de::Deserialize::deserialize(&mut *self.core).map_err(|e: Error| e.in_key(&name))
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(mut self, seed: T) -> Result<T::Value, Error> {
        self.reading();
        let name = self.name;
        seed.deserialize(&mut *self.core)
            .map_err(|e| e.in_key(&name))
    }

    fn tuple_variant<V: Visitor<'de>>(mut self, _len: usize, v: V) -> Result<V::Value, Error> {
        self.reading();
        let name = self.name;
        self.core.node(Hint::Seq, v).map_err(|e| e.in_key(&name))
    }

    fn struct_variant<V: Visitor<'de>>(
        mut self,
        _fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        self.reading();
        let name = self.name;
        self.core.node(Hint::Map, v).map_err(|e| e.in_key(&name))
    }
}
