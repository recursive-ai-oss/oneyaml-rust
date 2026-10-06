//! `serde::Deserializer` for [`Core`]: dispatch on the next node and the
//! target's [`Hint`].

use oneyaml_core::{Span, code};
use serde::de::{self, DeserializeSeed, Unexpected, Visitor};

use super::access::{EnumState, MapAccess, MapEnum, SeqAccess, TaggedCollection, scalar_key};
use std::rc::Rc;

use super::scalar::{IntTy, Kind, Sc, Variants, variant_name};
use super::{Core, Hint, is_target_error};
use crate::construct::build::{Cx, build_from};
use crate::construct::keys::{KeyRepr, KeySet, duplicate_at, key_text};
use crate::construct::scalar::resolve_collection;
use crate::construct::{Ev, Node, Pull, Tag};
use crate::error::Error;
use crate::resolve::{CollectionKind, ResolvedCollection};

impl<'de, P: Pull<'de>> Core<'de, P> {
    /// A custom-tagged scalar gets the read's memo (a typed enum
    /// matches its tag against its variant names with it; a name
    /// joined from a `%TAG` tag is charged to the read's byte budget).
    pub(crate) fn with_memo(&self, mut sc: Sc<'de>) -> Sc<'de> {
        if matches!(sc.kind, Kind::Custom(_)) {
            sc.memo = Some(Rc::clone(&self.variants));
        }
        sc
    }

    /// Consumes one whole node iteratively (ignored content never recurses).
    /// Both readers agree: content that was not planned yet is checked as the
    /// `Value` builder would (scalars and tags resolved, duplicate and merge
    /// keys); planned output was checked when it was planned.
    pub(crate) fn skip_node(&mut self) -> Result<(), Error> {
        let first = self.src.next()?;
        if !self.src.last_planned {
            return super::plan::check_node(
                &mut self.src,
                first,
                self.resolver,
                self.policy,
                &mut self.warnings,
                &mut self.scratch,
            );
        }
        let mut open = 0usize;
        let mut ev = first;
        loop {
            match ev.node {
                Node::SeqStart { .. } | Node::MapStart { .. } => open = open.saturating_add(1),
                Node::SeqEnd | Node::MapEnd => open = open.saturating_sub(1),
                Node::Scalar { .. } => {}
                _ => return Err(Error::internal("node expected", Some(ev.span))),
            }
            if open == 0 {
                return Ok(());
            }
            ev = self.src.next()?;
        }
    }

    /// Builds the next node as a `Value` with the shared builder.
    pub(crate) fn build_value(&mut self) -> Result<crate::Value, Error> {
        let first = self.src.next()?;
        let mut cx = Cx {
            resolver: self.resolver,
            policy: self.policy,
            warnings: &mut self.warnings,
        };
        build_from(&mut self.src, &mut cx, first)
    }

    pub(crate) fn node<V: Visitor<'de>>(&mut self, hint: Hint, v: V) -> Result<V::Value, Error> {
        self.check_unpoisoned()?;
        let span = self.src.peek()?.span;
        let depth = self.src.open;
        let result = self.node_unchecked(hint, v);
        if let Err(e) = &result {
            self.after_error(e, span, depth);
        }
        result
    }

    /// After the read of a node (that started at `span`, `depth` open
    /// collections) failed with `e`. Each failed collection read
    /// consumes and checks its unread rest before it returns (the
    /// `visit_*` error paths), so a target may ignore a target error
    /// (`Y3000`-`Y3999`: `serde_with::DefaultOnError`, `.ok()`) and read
    /// on, in step. The read is poisoned (`Y3018` on the next
    /// read and at the end) only when that was impossible (the rest is
    /// still unread: it holds an error itself) or when `e` is the input's
    /// own error (syntax, meaning, limits): ignoring it must not turn an
    /// input the `Value` read refuses into a value.
    pub(crate) fn after_error(&mut self, e: &Error, span: Span, depth: usize) {
        if self.src.open > depth || !is_target_error(e) {
            self.poison(span);
        }
    }

    fn node_unchecked<V: Visitor<'de>>(&mut self, hint: Hint, v: V) -> Result<V::Value, Error> {
        if hint == Hint::Ignored {
            self.skip_node()?;
            return v.visit_unit();
        }
        let ev = self.src.next()?;
        let span = ev.span;
        match ev.node {
            Node::Scalar { value, plain, tag } => {
                let sc = self.with_memo(
                    Sc::classify(&self.resolver, tag, plain, value, span)?
                        .typed(self.typed_depth, self.max_typed_depth),
                );
                sc.visit(hint, v).map_err(|e| e.at(span))
            }
            Node::SeqStart { tag } => self
                .collection(CollectionKind::Sequence, tag, span, hint, v)
                .map_err(|e| e.at(span)),
            Node::MapStart { tag } => self
                .collection(CollectionKind::Mapping, tag, span, hint, v)
                .map_err(|e| e.at(span)),
            _ => Err(Error::internal("node expected", Some(span))),
        }
    }

    fn collection<V: Visitor<'de>>(
        &mut self,
        kind: CollectionKind,
        tag: Tag<'de>,
        span: Span,
        hint: Hint,
        v: V,
    ) -> Result<V::Value, Error> {
        let (open, reads) = (self.src.open, self.src.reads);
        let result = self.collection_unchecked(kind, tag, span, hint, v);
        if result.is_err() && !self.is_poisoned() && self.src.reads == reads {
            // Refused before anything inside was read (a type
            // mismatch, an unknown variant): the whole node is skipped and
            // checked, as ignored content.
            if self.src.open == open {
                let node = match kind {
                    CollectionKind::Sequence => Node::SeqStart { tag: Tag::Absent },
                    CollectionKind::Mapping => Node::MapStart { tag: Tag::Absent },
                };
                self.src.push_front(Ev::new(node, span));
            }
            // On failure the rest is unread (or holds the input's error):
            // the read is poisoned.
            if self.skip_node().is_err() {
                self.poison(span);
            }
        }
        result
    }

    fn collection_unchecked<V: Visitor<'de>>(
        &mut self,
        kind: CollectionKind,
        tag: Tag<'de>,
        span: Span,
        hint: Hint,
        v: V,
    ) -> Result<V::Value, Error> {
        // Read right after the node's event (taken in `node`).
        let prepaid = self.src.last_prepaid;
        let custom = match resolve_collection(&self.resolver, &tag, kind, span)? {
            ResolvedCollection::Plain => None,
            ResolvedCollection::Custom(_) => match &tag {
                Tag::Named(t) => Some(t.clone()),
                _ => None,
            },
        };
        let start = |tag: Tag<'de>| {
            let node = match kind {
                CollectionKind::Sequence => Node::SeqStart { tag },
                CollectionKind::Mapping => Node::MapStart { tag },
            };
            Ev::new(node, span)
        };
        match hint {
            Hint::Option => {
                return self.reenter(span, start(tag), |c| v.visit_some(c));
            }
            Hint::Newtype => {
                return self.reenter(span, start(tag), |c| v.visit_newtype_struct(c));
            }
            _ => {}
        }
        if let Some(t) = custom {
            // `!Name [..]` / `!Name {..}`: an enum variant with that content.
            return match hint {
                Hint::Any | Hint::Enum(_) => {
                    let variants = match hint {
                        Hint::Enum(names) => Some(names),
                        _ => None,
                    };
                    let shared = Rc::clone(&self.variants);
                    let name = variant_name(
                        t.tag(),
                        variants.map(|names| Variants {
                            names,
                            memo: &shared.variants,
                        }),
                        Some(&shared.meter),
                        span,
                    )?;
                    self.src.push_front(start(Tag::Absent));
                    let reads = self.src.reads;
                    let result = v.visit_enum(TaggedCollection::new(self, name));
                    if !self.is_poisoned() && self.src.reads == reads {
                        // The content was never read (an
                        // unknown variant, refused or ignored): skipped and
                        // checked as ignored content, so the read goes on in
                        // step, as `from_value` does. If that fails, the
                        // read is poisoned.
                        if let Err(e) = self.skip_node() {
                            self.poison(span);
                            return Err(result.err().unwrap_or(e));
                        }
                    }
                    result
                }
                _ => Err(de::Error::invalid_type(
                    Unexpected::Other(what(kind, true)),
                    &v,
                )),
            };
        }
        let visit = match (kind, hint) {
            (CollectionKind::Sequence, Hint::Any | Hint::Seq) => Self::visit_sequence,
            (CollectionKind::Mapping, Hint::Any | Hint::Map) => Self::visit_mapping,
            (CollectionKind::Mapping, Hint::Enum(_)) => Self::visit_map_enum,
            (CollectionKind::Sequence, _) => {
                return Err(de::Error::invalid_type(Unexpected::Seq, &v));
            }
            (CollectionKind::Mapping, _) => {
                return Err(de::Error::invalid_type(Unexpected::Map, &v));
            }
        };
        // The visitor recurses into its children on the native stack
        // (serde's model), so bound how deep that can follow the input. A
        // re-entry that brought the target here already counted the level.
        if prepaid.is_some() && prepaid == super::level_of(self.typed_depth) {
            return visit(self, v);
        }
        if self.typed_depth >= self.max_typed_depth {
            return Err(typed_depth_exceeded(self.max_typed_depth, Some(span)));
        }
        self.typed_depth = self.typed_depth.saturating_add(1);
        let result = visit(self, v);
        self.typed_depth = self.typed_depth.saturating_sub(1);
        result
    }

    /// Reads the same node again through the target's own `Deserialize`
    /// (an option, a newtype, `Spanned`): one typed level more (bounded by
    /// `max_typed_depth`, never a stack overflow), as for a collection.
    ///
    /// `ev`, the node's event, goes back marked with the raised level: a
    /// collection visit of this node *inside* the re-entry is the same
    /// level (`Newtype(Vec<T>)` is one level, like `Vec<T>`). The mark is
    /// honoured only at that level and cleared when the re-entry ends
    /// unread.
    fn reenter<R>(
        &mut self,
        span: Span,
        ev: Ev<'de>,
        f: impl FnOnce(&mut Self) -> Result<R, Error>,
    ) -> Result<R, Error> {
        if self.typed_depth >= self.max_typed_depth {
            return Err(typed_depth_exceeded(self.max_typed_depth, Some(span)));
        }
        let level = self.typed_depth.saturating_add(1);
        self.typed_depth = level;
        self.src.push_front_prepaid(ev, level);
        let result = f(self);
        let unread = self.src.end_prepaid(level);
        self.typed_depth = self.typed_depth.saturating_sub(1);
        if result.is_err() && unread && !self.is_poisoned() {
            // Refused unread, the node is skipped and checked as
            // ignored content, so a target that ignores the error reads on
            // in step; if that fails, the read is out of step.
            if self.skip_node().is_err() {
                self.poison(span);
            }
        }
        let value = result?;
        if unread {
            // The visitor took the node without reading it: it is consumed
            // (skipped, checked as ignored content), as a `Value` read
            // consumes it, never re-read by the next reader.
            self.skip_node()?;
        }
        Ok(value)
    }

    fn visit_sequence<V: Visitor<'de>>(&mut self, v: V) -> Result<V::Value, Error> {
        let mut access = SeqAccess::new(self);
        let value = match v.visit_seq(&mut access) {
            Ok(value) => value,
            Err(e) => {
                // The unread items are skipped and checked, each
                // as ignored content (see `after_error`).
                if !access.finish().0
                    && !self.is_poisoned()
                    && self.skip_rest_of_sequence().is_err()
                {
                    self.poison_here();
                }
                return Err(e);
            }
        };
        let (done, taken) = access.finish();
        if done {
            return Ok(value);
        }
        // A visitor that ignored a poisoning error and returned:
        // `Y3018`, not the target's verdict on the count.
        self.check_unpoisoned()?;
        // The visitor stopped early (a fixed-size tuple): count the rest.
        let mut extra = 0usize;
        loop {
            if matches!(self.src.peek()?.node, Node::SeqEnd) {
                self.src.next()?;
                break;
            }
            self.skip_node()?;
            extra = extra.saturating_add(1);
        }
        if extra == 0 {
            Ok(value)
        } else {
            Err(de::Error::invalid_length(
                taken.saturating_add(extra),
                &format!("{taken} elements").as_str(),
            ))
        }
    }

    /// Poisons the read where it stands (a failed skip).
    fn poison_here(&mut self) {
        self.poison(Span::empty(self.src.last_end));
    }

    /// Skips the rest of the sequence being read, its end included, each
    /// item checked as ignored content.
    fn skip_rest_of_sequence(&mut self) -> Result<(), Error> {
        loop {
            if matches!(self.src.peek()?.node, Node::SeqEnd) {
                self.src.next()?;
                return Ok(());
            }
            self.skip_node()?;
        }
    }

    /// Merge keys or tolerated duplicates: buffers the mapping just
    /// opened and rewrites its entries, and every mapping nested in them,
    /// per the shared rules (keys.rs), in one pass. The output has no `<<`
    /// and no duplicates and is marked planned, so nested mappings are not
    /// buffered again (O(n), not O(n × depth)).
    fn plan_mapping(&mut self) -> Result<(), Error> {
        if self.needs_plan() && !self.src.last_planned {
            let planned = super::plan::plan(
                &mut self.src,
                self.resolver,
                self.policy,
                &mut self.warnings,
            )?;
            self.src.push_front_all(planned, true);
        }
        Ok(())
    }

    fn visit_mapping<V: Visitor<'de>>(&mut self, v: V) -> Result<V::Value, Error> {
        self.plan_mapping()?;
        let mut access = MapAccess::new(self);
        match v.visit_map(&mut access) {
            // The visitor stopped before the end: the rest is ignored,
            // checked as the builder checks it (duplicates of the keys read
            // included).
            Ok(value) => access.finish_rest().map(|()| value),
            Err(e) => {
                // As an ignored rest (see `after_error`).
                if access.core_unpoisoned() && access.finish_rest().is_err() {
                    self.poison_here();
                }
                Err(e)
            }
        }
    }

    /// Reads the rest of the mapping being read, its end included, as the
    /// `Value` builder would: each key resolved (a collection key
    /// built), each value checked as ignored content, and each key checked
    /// against `keys` (the keys read before) under the duplicate-key
    /// policy. The builder's order: a value's errors before its key's
    /// duplicate error.
    ///
    /// Returns how many entries it read.
    pub(crate) fn finish_mapping(&mut self, keys: &mut KeySet<'de>) -> Result<usize, Error> {
        self.check_unpoisoned()?;
        let mut entries = 0usize;
        loop {
            let ev = self.src.next()?;
            let (span, replayed) = (ev.span, ev.replayed);
            let (repr, text) = match ev.node {
                Node::MapEnd => return Ok(entries),
                Node::Scalar { value, plain, tag } => {
                    let sc = Sc::classify(&self.resolver, tag, plain, value, span)?;
                    let text = sc.value.clone().into_owned();
                    (scalar_key(&sc), text)
                }
                Node::SeqStart { .. } | Node::MapStart { .. } => {
                    let mut cx = Cx {
                        resolver: self.resolver,
                        policy: self.policy,
                        warnings: &mut self.warnings,
                    };
                    let key = build_from(&mut self.src, &mut cx, ev)?;
                    let text = key_text(&key);
                    (KeyRepr::of_value(key), text)
                }
                _ => return Err(Error::internal("mapping key expected", Some(span))),
            };
            self.skip_node().map_err(|e| e.in_key(&text))?;
            entries = entries.saturating_add(1);
            if let Some(first) = keys.insert(repr, span) {
                let policy = self.policy;
                duplicate_at(&mut self.warnings, policy, replayed, || text, first, span)?;
            }
        }
    }

    fn visit_map_enum<V: Visitor<'de>>(&mut self, v: V) -> Result<V::Value, Error> {
        // Fuzz finding: the variant mapping means what the
        // `Value` reads — duplicates resolved (LastWins), merges applied —
        // before its single key is checked.
        self.plan_mapping()?;
        let open = self.src.open;
        let mut state = EnumState::default();
        let result = v.visit_enum(MapEnum::new(self, &mut state));
        if self.is_poisoned() {
            // A value after an ignored poisoning error is `Y3018`.
            return result.and_then(|v| self.check_unpoisoned().map(|()| v));
        }
        // The rest of the mapping is read as the
        // `Value` builder reads it (values checked, keys checked for
        // duplicates against the variant key) before the mapping is judged
        // as a variant: an error there is the read's first error when the
        // variant key was refused or a second entry follows, as it is
        // `Value`'s and a strict `Document`'s (`Y1013` in
        // `{Unknown: 1, Unknown: 2}`, `Y1010` in `!Square 2.0: x` under
        // `Schema::Json`).
        let mut keys = KeySet::default();
        let key_read = state.key.is_some();
        if let Some((repr, span)) = state.key.take() {
            keys.insert(repr, span);
        }
        match result {
            // The mapping's end is read (`{}`, its error ignored).
            Ok(value) if self.src.open < open => Ok(value),
            Ok(value) => {
                // A visitor that ignored the variant's error (or
                // never read the content) leaves the content unread: it is
                // skipped and checked, as ignored content.
                if let Some(name) = state.content_due.take() {
                    self.skip_node().map_err(|e| e.in_key(&name))?;
                }
                if matches!(self.src.peek()?.node, Node::MapEnd) {
                    self.src.next()?;
                    return Ok(value);
                }
                let entries = self.finish_mapping(&mut keys)?;
                // A variant key left unread (a collection, refused and
                // ignored) is one of these entries: a single one is the
                // whole mapping, as `from_value` counts it.
                if !key_read && entries <= 1 {
                    return Ok(value);
                }
                Err(de::Error::invalid_length(
                    2,
                    &"a mapping with a single key (the variant)",
                ))
            }
            // The mapping's end is read (`{}`).
            Err(e) if self.src.open < open => Err(e),
            Err(e) => {
                let rest = match state.content_due.take() {
                    Some(name) => self
                        .skip_node()
                        .map_err(|e| e.in_key(&name))
                        .and_then(|()| self.finish_mapping(&mut keys)),
                    None => self.finish_mapping(&mut keys),
                };
                match rest {
                    Ok(_) => Err(e),
                    Err(first) => {
                        // The rest holds the input's error (or is unread):
                        // ignoring this error must not read on.
                        self.poison_here();
                        // The content's own error comes first (it is read
                        // in order, as a struct field is).
                        Err(if state.refused { first } else { e })
                    }
                }
            }
        }
    }
}

/// The `Y2007` limit error, shared by every typed reader so they
/// report it alike.
pub(crate) fn typed_depth_exceeded(limit: usize, span: Option<Span>) -> Error {
    Error::new(
        code::TYPED_DEPTH_EXCEEDED,
        format!("the target type nests deeper than {limit} levels here"),
        span,
    )
    .with_help(format!(
        "raise Limits::max_typed_depth (currently {limit}); each level of a recursive \
         type takes native stack (about 2 KiB in release, 8 KiB in debug builds), so \
         run on a thread with a large enough stack, or deserialize into `Value`, \
         which is iterative"
    ))
}

fn what(kind: CollectionKind, tagged: bool) -> &'static str {
    match (kind, tagged) {
        (CollectionKind::Sequence, true) => "tagged sequence",
        (CollectionKind::Mapping, true) => "tagged mapping",
        (CollectionKind::Sequence, false) => "sequence",
        (CollectionKind::Mapping, false) => "mapping",
    }
}

macro_rules! hinted {
    ($($method:ident => $hint:expr),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
            self.node($hint, v)
        }
    )*};
}

impl<'de, P: Pull<'de>> de::Deserializer<'de> for &mut Core<'de, P> {
    type Error = Error;

    hinted! {
        deserialize_any => Hint::Any,
        deserialize_bool => Hint::Bool,
        deserialize_i8 => Hint::Int(IntTy::I8),
        deserialize_i16 => Hint::Int(IntTy::I16),
        deserialize_i32 => Hint::Int(IntTy::I32),
        deserialize_i64 => Hint::Int(IntTy::I64),
        deserialize_i128 => Hint::Int(IntTy::I128),
        deserialize_u8 => Hint::Int(IntTy::U8),
        deserialize_u16 => Hint::Int(IntTy::U16),
        deserialize_u32 => Hint::Int(IntTy::U32),
        deserialize_u64 => Hint::Int(IntTy::U64),
        deserialize_u128 => Hint::Int(IntTy::U128),
        deserialize_f32 => Hint::Float,
        deserialize_f64 => Hint::Float,
        deserialize_char => Hint::Str,
        deserialize_str => Hint::Str,
        deserialize_string => Hint::Str,
        deserialize_bytes => Hint::Str,
        deserialize_byte_buf => Hint::Str,
        deserialize_option => Hint::Option,
        deserialize_unit => Hint::Unit,
        deserialize_seq => Hint::Seq,
        deserialize_map => Hint::Map,
        deserialize_identifier => Hint::Str,
        deserialize_ignored_any => Hint::Ignored,
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        self.node(Hint::Unit, v)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        if name == crate::value::VALUE_MAGIC {
            // `Value` is built iteratively by the shared builder,
            // never through the recursive visitor protocol.
            self.check_unpoisoned()?;
            let span = self.src.peek()?.span;
            let depth = self.src.open;
            let value = match self.build_value() {
                Ok(v) => v,
                Err(e) => {
                    // As `node`: the builder's errors are the
                    // input's, so ignoring one poisons the read.
                    self.after_error(&e, span, depth);
                    return Err(e);
                }
            };
            return crate::value::hand_off(value, v).map_err(|e| e.at(span));
        }
        self.node(Hint::Newtype, v)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, v: V) -> Result<V::Value, Error> {
        self.node(Hint::Seq, v)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: usize,
        v: V,
    ) -> Result<V::Value, Error> {
        self.node(Hint::Seq, v)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        if crate::spanned::is_spanned(name, fields) {
            // The node's event goes back marked prepaid: `Spanned<Vec<T>>`
            // is one typed level, like `Vec<T>`.
            let ev = self.src.next()?;
            let span = ev.span;
            let start = span.start;
            return self.reenter(span, ev, |c| {
                v.visit_map(crate::spanned::SpannedAccess::new(start, start, c))
            });
        }
        self.node(Hint::Map, v)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        self.node(Hint::Enum(variants), v)
    }
}

impl<'de, P: Pull<'de>> crate::spanned::EndAfter<'de> for &mut Core<'de, P> {
    fn value_and_end<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Option<usize>), Error> {
        let value = seed.deserialize(&mut *self)?;
        Ok((value, Some(self.src.last_end())))
    }
}
