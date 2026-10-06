//! `serde::Deserializer` for [`Value`]: `from_value` (moving
//! values between serde and the edit tree").
//!
//! # One interpretation
//!
//! A `Value` is already resolved: the one resolver gave every scalar its
//! meaning when the value was built, so reading it never resolves again.
//! Delivery to the target type follows the streaming deserializer
//! exactly: scalars go through the same [`Sc::visit`] dispatch (the same
//! coercion rules, messages, and `Y####` codes), and collections, tagged
//! values, enums, options, and newtypes mirror `Core`'s dispatch arm by
//! arm (`de/core.rs`, `de/access.rs`, `de/scalar.rs`), including the
//! `max_typed_depth` bound (`Y2007`).
//!
//! **The one necessary difference.** The streaming deserializer reads a *plain,
//! untagged* scalar into a string target as its source text (`8080` →
//! `"8080"`). A `Value` has no source text and no scalar style:
//! `Value::Number(8080)` does not know it was written plain, or as `8080`
//! rather than `0x1F90`. Reading a plain scalar into a string is "a
//! type-directed read of the same node", and there is no node here; inventing a
//! text (`1.10` → `"1.1"`) would be a second interpretation. So a `Value` reads
//! like an explicitly tagged scalar does (`!!int 8080` → `String` is a type
//! mismatch): `Null`, `Bool`, and `Number` never go into string targets
//! (`Y3000`), exactly as `serde_yaml::from_value` behaves.
//! [`NodeRef::deserialize`] reads the syntax tree instead and keeps that
//! reading.
//!
//! **The second difference (the same cause).** Under
//! `Settings::resolve_tagged_variant_content`, a typed enum reads the
//! content of a *plain* locally tagged scalar resolved under Core
//! (`!Square 2.0` → `Square(2.0)`), and quoted content stays a string. A
//! `Value` keeps no scalar style: `!Square '2.0'` and `!Square 2.0` are the
//! same `Tagged("!Square", String("2.0"))`, so `from_value_with` resolves
//! the content of every tagged string a typed enum reads, to a bool or a
//! number only: never to null (`!Opt 'null'` and `!Opt null` are the same
//! `Value`, and `Opt(Some("null"))` must not read back as `Opt(None)`), and
//! string targets still read the text.
//!
//! Everything else agrees with `from_str` (tests/value_de.rs, and the
//! corpus differential in the bench crate).
//!
//! [`NodeRef::deserialize`]: crate::NodeRef::deserialize
//!
//! # Recursion
//!
//! As for the streaming deserializer, the native stack follows only the
//! target type: each collection visited through a typed visitor counts one
//! level of `max_typed_depth`, and so does each tag of a chain of tags
//! (which only a programmatic `Value` can hold). A `Value` target is
//! handed over whole (a clone, or the value itself for [`from_value`]),
//! never visited. The value's `max_depth` is checked first, iteratively
//! (`Y2005`).
//!
//! # Errors
//!
//! Errors carry the code, message, and path (`spec.ports[2].port`); a
//! `Value` has no spans, so [`Error::span`] and [`Error::location`] are
//! `None`. Path segments for non-string keys are the key's canonical form
//! (`16` for a key written `0x10`).

use std::borrow::Cow;
use std::marker::PhantomData;

use oneyaml_core::{Limits, Settings, Span};
use serde::de::value::StrDeserializer;
use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, Unexpected, Visitor};
use std::cell::{Cell, RefCell};

use super::Hint;
use super::core::typed_depth_exceeded;
use super::scalar::{IntTy, Kind, Sc, ScalarDe, VariantMemo, Variants, variant_name};
use crate::construct::keys::key_text;
use crate::construct::scalar::resolve_error;
use crate::error::Error;
use crate::resolve::{Resolver, Style};
use crate::value::{
    Mapping, MappingIter, Sequence, TagStr, TaggedValue, VALUE_MAGIC, Value, hand_off,
};

/// Deserializes a `T` from a [`Value`], with default [`Limits`].
///
/// The value is read exactly as [`from_str`](crate::from_str) reads the
/// same data (the same coercion rules and `Y####` codes), with one
/// necessary difference: a `Value` has no source text, so `Null`, `Bool`,
/// and `Number` never go into a string target. `from_str` reads a
/// *plain scalar* into a string as its *source text* (`from_str` gives
/// `"1.10"` for `1.10`); a `Value` holds only the resolved meaning
/// (`1.1`), so a made-up text would be a second interpretation. A `Value`
/// reads as an explicitly tagged scalar does (`!!int 8080` into `String`
/// is a type mismatch). This mirrors `serde_yaml::from_value`; to keep
/// the source text, read the node itself with
/// [`NodeRef::deserialize`](crate::NodeRef::deserialize). A `Value`
/// target takes the value itself.
///
/// ```
/// use std::collections::BTreeMap;
///
/// #[derive(Debug, PartialEq, serde::Deserialize)]
/// struct Port {
///     name: String,
///     port: u16,
/// }
/// let v: oneyaml::Value = oneyaml::from_str("name: http\nport: 8080\n").unwrap();
/// let p: Port = oneyaml::from_value(v.clone()).unwrap();
/// assert_eq!(p, Port { name: "http".into(), port: 8080 });
///
/// // A resolved number is not a string (`from_str` would read the text):
/// let e = oneyaml::from_value::<BTreeMap<String, String>>(v).unwrap_err();
/// assert_eq!(e.code(), oneyaml::code::TYPE_MISMATCH);
/// assert_eq!(e.to_string(), "invalid type: integer `8080`, expected a string at port");
/// assert_eq!(e.span(), None);
/// ```
///
/// # Errors
///
/// Type mismatches and the target's own errors (`Y3xxx`), with their path;
/// `Y2005` when the value nests deeper than `Limits::max_depth`; `Y2007`
/// when a recursive target type nests deeper than
/// `Limits::max_typed_depth`.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, Error> {
    T::deserialize(OwnedRoot {
        value,
        limits: Limits::default(),
        resolver: Resolver::new(&Settings::new()),
    })
}

/// [`from_value`] with explicit [`Settings`]. The limits apply
/// (`max_depth`, `max_typed_depth`), and so does
/// [`Settings::resolve_tagged_variant_content`]: a typed enum reads
/// `Tagged("!Square", String("2.0"))` as `Square(2.0)`, as `from_str_with`
/// reads `!Square 2.0`. Otherwise the value was resolved when it was
/// built, and schema settings have nothing left to decide.
///
/// Two documented differences from `from_str_with`, both because a `Value`
/// keeps a scalar's meaning, not how it was written: `Null`, `Bool`, and
/// `Number` never go into a string target (see [`from_value`]); and under
/// `resolve_tagged_variant_content`, **quoted** tagged content resolves
/// too: `from_str_with` refuses `!Square '2.0'` as a `Square(f64)`, while
/// its `Value`, the same `Tagged("!Square", String("2.0"))` as that of
/// `!Square 2.0`, reads as `Square(2.0)` here. The content resolves to a
/// bool or a number only, **never to null**: `!Opt 'null'`, `!Opt ''` and
/// `!Opt null` are the same `Value`, so here each reads as
/// `Opt(Some("null"))`/`Opt(Some(""))` into an `Option<String>` (text never
/// becomes `None`), and a `Tagged("!Maybe", String(""))` into an
/// `Option<u8>` is a type mismatch, where `from_str_with` reads
/// `!Maybe null` as `Maybe(None)`.
///
/// ```
/// use oneyaml::{Limits, Settings};
///
/// let v: oneyaml::Value = oneyaml::from_str("[[[1]]]").unwrap();
/// let shallow = Settings::new().limits(Limits::default().max_depth(2));
/// let e = oneyaml::from_value_with::<Vec<Vec<Vec<u8>>>>(v.clone(), &shallow).unwrap_err();
/// assert_eq!(e.code(), oneyaml::code::VALUE_DEPTH_EXCEEDED);
/// let n: Vec<Vec<Vec<u8>>> = oneyaml::from_value_with(v, &Settings::new()).unwrap();
/// assert_eq!(n, [[[1]]]);
/// ```
///
/// # Errors
///
/// As [`from_value`].
pub fn from_value_with<T: DeserializeOwned>(value: Value, settings: &Settings) -> Result<T, Error> {
    T::deserialize(OwnedRoot {
        value,
        limits: settings.get_limits().clone(),
        resolver: Resolver::new(settings),
    })
}

/// How strings reach the visitor: borrowed from a `&'de Value`, or copied
/// out of a value the deserializer owns.
pub(crate) trait Text<'a, 'de>: Copy {
    fn text(s: &'a str) -> Cow<'de, str>;
}

/// Strings borrowed from the value (`&'de Value`).
#[derive(Clone, Copy)]
pub(crate) struct Borrowed;

impl<'de> Text<'de, 'de> for Borrowed {
    fn text(s: &'de str) -> Cow<'de, str> {
        Cow::Borrowed(s)
    }
}

/// Strings copied (an owned `Value`).
#[derive(Clone, Copy)]
pub(crate) struct Copied;

impl<'de> Text<'_, 'de> for Copied {
    fn text(s: &str) -> Cow<'de, str> {
        Cow::Owned(s.to_owned())
    }
}

/// A deserializer over one node of a `Value`: `depth` collections (and
/// chained tags) are open through typed visitors above it.
pub(crate) struct ValueDe<'a, 'm, 'de, M> {
    value: &'a Value,
    depth: usize,
    max: usize,
    /// A re-entry of this node (an option, a newtype) counted its typed
    /// level: visiting it as a collection costs nothing more (`Core`'s
    /// prepaid events).
    prepaid: bool,
    /// For `Settings::resolve_tagged_variant_content` only: a `Value` is
    /// never resolved again otherwise.
    resolver: Resolver,
    /// The read's variant memo.
    memo: &'m RefCell<VariantMemo>,
    /// The read returned an error that poisons it (`Core`'s
    /// `poisoned`): reading on is `Y3018`, as it is for `from_str`.
    poisoned: &'m Cell<bool>,
    mode: PhantomData<fn() -> (M, &'de ())>,
}

impl<M> Clone for ValueDe<'_, '_, '_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for ValueDe<'_, '_, '_, M> {}

/// Checks the value's depth, then reads it from the root.
fn run<'a, 'de, M: Text<'a, 'de>, R>(
    value: &'a Value,
    limits: &Limits,
    resolver: Resolver,
    f: impl for<'m> FnOnce(ValueDe<'a, 'm, 'de, M>) -> Result<R, Error>,
) -> Result<R, Error> {
    if let Err(e) = value.check_depth(limits) {
        return Err(e.into_error().finish(None, Vec::new()));
    }
    let memo = RefCell::default();
    let poisoned = Cell::new(false);
    let read = f(ValueDe::new(
        value,
        0,
        limits.get_max_typed_depth(),
        resolver,
        &memo,
        &poisoned,
    ));
    // A target that ignored an error and returned a value (the
    // end-of-document check of `from_str`).
    match read {
        Ok(_) if poisoned.get() => Err(finish(super::read_after_ignored_error(None))),
        other => other.map_err(finish),
    }
}

/// The entry point's last step: no span (a `Value` has none), the path.
fn finish(e: Error) -> Error {
    e.without_span().finish(None, Vec::new())
}

/// A key as a path segment: a string, or the text of a (tagged) string
/// scalar as the streaming deserializer shows it, else its canonical form.
fn key_name(k: &Value) -> String {
    match k {
        Value::String(s) => crate::error::segment(s),
        Value::Tagged(t) => match &t.value {
            Value::String(s) => crate::error::segment(s),
            _ => key_text(k),
        },
        _ => key_text(k),
    }
}

/// What a tagged value is, for type mismatches (as `Core` names it).
fn tagged_what(content: &Value) -> &'static str {
    match content {
        Value::Sequence(_) => "tagged sequence",
        Value::Mapping(_) => "tagged mapping",
        Value::Tagged(_) => "tagged value",
        _ => "tagged scalar",
    }
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> ValueDe<'a, 'm, 'de, M> {
    fn new(
        value: &'a Value,
        depth: usize,
        max: usize,
        resolver: Resolver,
        memo: &'m RefCell<VariantMemo>,
        poisoned: &'m Cell<bool>,
    ) -> Self {
        Self {
            value,
            depth,
            max,
            prepaid: false,
            resolver,
            memo,
            poisoned,
            mode: PhantomData,
        }
    }

    fn at(self, value: &'a Value, depth: usize) -> Self {
        Self::new(
            value,
            depth,
            self.max,
            self.resolver,
            self.memo,
            self.poisoned,
        )
    }

    /// One more level through a typed visitor: `Y2007` past the
    /// limit, as `Core::collection` reports it.
    fn enter(self) -> Result<usize, Error> {
        if self.depth >= self.max {
            Err(typed_depth_exceeded(self.max, None))
        } else {
            Ok(self.depth.saturating_add(1))
        }
    }

    /// The same node read again through the target's own `Deserialize`
    /// (an option, a newtype, a tagged scalar's content): one typed level
    /// more, as `Core` counts it.
    fn reentered(self) -> Result<Self, Error> {
        let mut again = self.at(self.value, self.enter()?);
        again.prepaid = true;
        Ok(again)
    }

    /// The depth for this node's children when it is visited as a
    /// collection: one level more, unless a re-entry already paid it.
    fn visit_level(self) -> Result<usize, Error> {
        if self.prepaid {
            Ok(self.depth)
        } else {
            self.enter()
        }
    }

    /// `Y3018` once the target ignored an error that poisons the read
    /// (`Core::check_unpoisoned`).
    fn check_unpoisoned(self) -> Result<(), Error> {
        if self.poisoned.get() {
            Err(super::read_after_ignored_error(None))
        } else {
            Ok(())
        }
    }

    /// `Core::node`: dispatch on the node and the target's hint. As there
    /// (`Core::after_error`), a target error leaves the read
    /// usable, and any other error (a limit) poisons it. A `Value` holds no
    /// input errors and has nothing left unread, so `Core`'s other reason
    /// never arises here.
    fn node<V: Visitor<'de>>(self, hint: Hint, v: V) -> Result<V::Value, Error> {
        self.check_unpoisoned()?;
        let result = self.node_unchecked(hint, v);
        if let Err(e) = &result {
            if !super::is_target_error(e) {
                self.poisoned.set(true);
            }
        }
        result
    }

    fn node_unchecked<V: Visitor<'de>>(self, hint: Hint, v: V) -> Result<V::Value, Error> {
        if hint == Hint::Ignored {
            // The value is already checked: nothing to skip.
            return v.visit_unit();
        }
        match self.value {
            Value::Null => self.scalar(Kind::Null, Cow::Borrowed(""), hint, v),
            Value::Bool(b) => self.scalar(Kind::Bool(*b), Cow::Borrowed(""), hint, v),
            Value::Number(n) => {
                // The text only appears in messages (`integer … is out of
                // range`): a number never goes into a string target here.
                let text = if matches!(hint, Hint::Int(_) | Hint::Float | Hint::Any) {
                    Cow::Owned(n.to_string())
                } else {
                    Cow::Borrowed("")
                };
                self.scalar(Kind::Number(n.clone()), text, hint, v)
            }
            Value::String(s) => self.scalar(Kind::Str, M::text(s), hint, v),
            Value::Sequence(q) => self.sequence(q, hint, v),
            Value::Mapping(m) => self.mapping(m, hint, v),
            Value::Tagged(t) => self.tagged(t, hint, v),
        }
    }

    /// `Sc::visit`, with the resolved kind and **no** string leniency (see
    /// the module docs). Options and newtypes wrap this deserializer, not
    /// a scalar one, so what they contain is read the same way.
    fn scalar<V: Visitor<'de>>(
        self,
        kind: Kind<'de>,
        text: Cow<'de, str>,
        hint: Hint,
        v: V,
    ) -> Result<V::Value, Error> {
        match hint {
            Hint::Option => match kind {
                Kind::Null => v.visit_none(),
                _ => v.visit_some(self.reentered()?),
            },
            Hint::Newtype => v.visit_newtype_struct(self.reentered()?),
            _ => Sc {
                kind,
                value: text,
                lenient: false,
                variant_content: None,
                // A placeholder: `finish` drops it (a `Value` has no span).
                span: Span::empty(0),
                depth: self.depth,
                max: self.max,
                memo: None,
            }
            .visit(hint, v),
        }
    }

    fn sequence<V: Visitor<'de>>(
        self,
        q: &'a Sequence,
        hint: Hint,
        v: V,
    ) -> Result<V::Value, Error> {
        match hint {
            Hint::Option => v.visit_some(self.reentered()?),
            Hint::Newtype => v.visit_newtype_struct(self.reentered()?),
            Hint::Any | Hint::Seq => {
                let depth = self.visit_level()?;
                let mut access = SeqDe {
                    de: self,
                    it: q.iter(),
                    taken: 0,
                    depth,
                };
                let value = v.visit_seq(&mut access)?;
                // `Core::visit_sequence`: a visitor that stopped early (a
                // fixed-size tuple) leaves elements over.
                let extra = access.it.len();
                if extra == 0 {
                    Ok(value)
                } else {
                    let taken = access.taken;
                    Err(de::Error::invalid_length(
                        taken.saturating_add(extra),
                        &format!("{taken} elements").as_str(),
                    ))
                }
            }
            _ => Err(de::Error::invalid_type(Unexpected::Seq, &v)),
        }
    }

    fn mapping<V: Visitor<'de>>(self, m: &'a Mapping, hint: Hint, v: V) -> Result<V::Value, Error> {
        match hint {
            Hint::Option => v.visit_some(self.reentered()?),
            Hint::Newtype => v.visit_newtype_struct(self.reentered()?),
            // A visitor that stops early ignores the rest, as `Core` does.
            Hint::Any | Hint::Map => v.visit_map(MapDe {
                de: self,
                it: m.iter(),
                entry: None,
                depth: self.visit_level()?,
            }),
            Hint::Enum(_) => {
                let depth = self.visit_level()?;
                let value = v.visit_enum(MapEnum {
                    de: self,
                    map: m,
                    depth,
                })?;
                // `Core::visit_map_enum`: checked after the variant.
                if m.len() > 1 {
                    return Err(de::Error::invalid_length(
                        2,
                        &"a mapping with a single key (the variant)",
                    ));
                }
                Ok(value)
            }
            _ => Err(de::Error::invalid_type(Unexpected::Map, &v)),
        }
    }

    /// A tagged value: an enum variant named by the tag (`!Name x`), as
    /// `Core` reads a custom-tagged scalar or collection.
    fn tagged<V: Visitor<'de>>(
        self,
        t: &'a TaggedValue,
        hint: Hint,
        v: V,
    ) -> Result<V::Value, Error> {
        match hint {
            Hint::Option => return v.visit_some(self.reentered()?),
            Hint::Newtype => return v.visit_newtype_struct(self.reentered()?),
            Hint::Any | Hint::Enum(_) => {}
            _ => {
                return Err(de::Error::invalid_type(
                    Unexpected::Other(tagged_what(&t.value)),
                    &v,
                ));
            }
        }
        // A chain of tags (a programmatic `Value` only; YAML
        // gives a node one tag) recurses once per tag through an enum
        // target, so each inner tag counts a level.
        let depth = match t.value {
            Value::Tagged(_) => self.enter()?,
            _ => self.depth,
        };
        let variants = match hint {
            Hint::Enum(names) => Some(Variants {
                names,
                memo: self.memo,
            }),
            _ => None,
        };
        // A `Value`'s size was bounded when it was read (or it is
        // the caller's own), and a joined name is at most its tag's
        // logical text: nothing new to charge.
        let name = variant_name(&t.tag, variants, None, Span::empty(0))?;
        let content = self.at(&t.value, depth);
        match &t.value {
            Value::String(s) => {
                // As `Sc::classify` + `TaggedScalar` read
                // `!Name text` in a typed enum under
                // `resolve_tagged_variant_content`. A `Value` keeps
                // no style: its string content is taken as written plain.
                // Resolved only by a newtype variant.
                let resolve = matches!(hint, Hint::Enum(_))
                    && self
                        .resolver
                        .resolves_variant_content(t.tag.parts(), Style::Plain);
                v.visit_enum(TaggedScalar {
                    name,
                    content,
                    text: s,
                    tag: t.tag.parts(),
                    resolve,
                })
            }
            _ => v.visit_enum(TaggedContent { name, content }),
        }
    }
}

macro_rules! hinted {
    ($($method:ident => $hint:expr),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
            self.node($hint, v)
        }
    )*};
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::Deserializer<'de> for ValueDe<'a, 'm, 'de, M> {
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
        if name == VALUE_MAGIC {
            // A `Value` target takes a clone (iterative), never the
            // recursive visitor protocol.
            return hand_off(self.value.clone(), v);
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
            // Never a made-up span.
            return Err(de::Error::invalid_type(
                Unexpected::Other("a Value (it has no source spans)"),
                &"a node read from YAML text (`Spanned<T>` needs `from_str` or \
                  `NodeRef::deserialize`)",
            ));
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

/// `SeqAccess` over a sequence's items (`de/access.rs` `SeqAccess`).
struct SeqDe<'a, 'm, 'de, M> {
    de: ValueDe<'a, 'm, 'de, M>,
    it: std::slice::Iter<'a, Value>,
    taken: usize,
    depth: usize,
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::SeqAccess<'de> for SeqDe<'a, 'm, 'de, M> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        self.de.check_unpoisoned()?;
        let Some(item) = self.it.next() else {
            return Ok(None);
        };
        let i = self.taken;
        self.taken = i.saturating_add(1);
        seed.deserialize(self.de.at(item, self.depth))
            .map(Some)
            .map_err(|e| e.in_index(i))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.it.len())
    }
}

/// `MapAccess` over a mapping's entries (`de/access.rs` `MapAccess`; the
/// value has no duplicate keys left to check).
struct MapDe<'a, 'm, 'de, M> {
    de: ValueDe<'a, 'm, 'de, M>,
    it: MappingIter<'a>,
    entry: Option<(&'a Value, &'a Value)>,
    depth: usize,
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::MapAccess<'de> for MapDe<'a, 'm, 'de, M> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        self.de.check_unpoisoned()?;
        let Some((k, v)) = self.it.next() else {
            return Ok(None);
        };
        self.entry = Some((k, v));
        seed.deserialize(self.de.at(k, self.depth)).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        self.de.check_unpoisoned()?;
        let Some((k, v)) = self.entry.take() else {
            debug_assert!(false, "next_value_seed before next_key_seed");
            return Err(Error::internal("mapping value read before its key", None));
        };
        seed.deserialize(self.de.at(v, self.depth))
            .map_err(|e| e.in_key(&key_name(k)))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.it.len())
    }
}

/// `{Variant: content}` (`de/access.rs` `MapEnum`).
struct MapEnum<'a, 'm, 'de, M> {
    de: ValueDe<'a, 'm, 'de, M>,
    map: &'a Mapping,
    depth: usize,
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::EnumAccess<'de> for MapEnum<'a, 'm, 'de, M> {
    type Error = Error;
    type Variant = Variant<'a, 'm, 'de, M>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Error> {
        let Some((k, content)) = self.map.get_index(0) else {
            return Err(de::Error::invalid_length(
                0,
                &"a mapping with a single key (the variant)",
            ));
        };
        if matches!(k.untag_ref(), Value::Sequence(_) | Value::Mapping(_)) {
            return Err(de::Error::invalid_type(
                Unexpected::Other("a collection as variant name"),
                &"a variant name",
            ));
        }
        let variant = seed.deserialize(self.de.at(k, self.depth))?;
        Ok((
            variant,
            Variant {
                content: self.de.at(content, self.depth),
                name: Cow::Owned(key_name(k)),
            },
        ))
    }
}

/// The content of a variant (`de/access.rs` `Variant`): errors inside it
/// are under the variant's name in the path.
struct Variant<'a, 'm, 'de, M> {
    content: ValueDe<'a, 'm, 'de, M>,
    name: Cow<'static, str>,
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::VariantAccess<'de> for Variant<'a, 'm, 'de, M> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        let name = self.name;
        de::Deserialize::deserialize(self.content).map_err(|e: Error| e.in_key(&name))
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        let name = self.name;
        seed.deserialize(self.content).map_err(|e| e.in_key(&name))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, v: V) -> Result<V::Value, Error> {
        let name = self.name;
        self.content.node(Hint::Seq, v).map_err(|e| e.in_key(&name))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        let name = self.name;
        self.content.node(Hint::Map, v).map_err(|e| e.in_key(&name))
    }
}

/// `!Name text` (`de/scalar.rs` `TaggedScalar`): the content is a string,
/// or, when `resolve` (a typed enum under `resolve_tagged_variant_content`),
/// the text resolved by a newtype variant.
struct TaggedScalar<'a, 'm, 'de, M> {
    name: Cow<'static, str>,
    content: ValueDe<'a, 'm, 'de, M>,
    text: &'a str,
    tag: TagStr<'a>,
    resolve: bool,
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::EnumAccess<'de> for TaggedScalar<'a, 'm, 'de, M> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Error> {
        let de: StrDeserializer<'_, Error> = (&*self.name).into_deserializer();
        let variant = seed.deserialize(de)?;
        Ok((variant, self))
    }
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::VariantAccess<'de> for TaggedScalar<'a, 'm, 'de, M> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        if self.text.is_empty() {
            Ok(())
        } else {
            Err(de::Error::invalid_type(
                Unexpected::Str(self.text),
                &"a unit variant (no content)",
            ))
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        let resolved = if self.resolve {
            self.content
                .resolver
                .variant_content(self.tag, Style::Plain, self.text)
                .map_err(|e| resolve_error(e, self.text, Span::empty(0)))?
        } else {
            None
        };
        // Never to Null here. A `Value` keeps
        // no style, and `!Opt 'null'` (what our own compat serializer
        // writes for `Opt(Some("null"))`) is the same `Value` as
        // `!Opt null`: resolving it to Null would turn text into `None`.
        // Bool and Number are safe: string targets still read the text.
        let kind = match resolved.and_then(Kind::plain) {
            Some(k @ (Kind::Bool(_) | Kind::Number(_))) => k,
            _ => return seed.deserialize(self.content.reentered()?),
        };
        // The resolved content, delivered exactly as `from_str` delivers it
        // (`de/scalar.rs` `ScalarContent`): the same `Sc` dispatch, and
        // the string's text for string targets (it is the text the
        // scalar was written as).
        let sc = Sc {
            kind,
            value: M::text(self.text),
            lenient: true,
            variant_content: None,
            // A placeholder: `finish` drops it (a `Value` has no span).
            span: Span::empty(0),
            depth: self.content.depth,
            max: self.content.max,
            memo: None,
        };
        seed.deserialize(ScalarDe(sc.reenter()?))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, v: V) -> Result<V::Value, Error> {
        Err(de::Error::invalid_type(Unexpected::Str(self.text), &v))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        Err(de::Error::invalid_type(Unexpected::Str(self.text), &v))
    }
}

/// `!Name [..]` / `!Name {..}` (`de/access.rs` `TaggedCollection`), and
/// any other tagged content a programmatic `Value` may hold.
struct TaggedContent<'a, 'm, 'de, M> {
    name: Cow<'static, str>,
    content: ValueDe<'a, 'm, 'de, M>,
}

impl<'a, 'm, 'de, M: Text<'a, 'de>> de::EnumAccess<'de> for TaggedContent<'a, 'm, 'de, M> {
    type Error = Error;
    type Variant = Variant<'a, 'm, 'de, M>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Error> {
        let de: StrDeserializer<'_, Error> = (&*self.name).into_deserializer();
        let variant = seed.deserialize(de)?;
        Ok((
            variant,
            Variant {
                content: self.content,
                name: self.name,
            },
        ))
    }
}

/// An owned `Value` at the root of a read, with the limits to apply.
struct OwnedRoot {
    value: Value,
    limits: Limits,
    resolver: Resolver,
}

/// The root methods of the public deserializers: the depth check first,
/// then the read, then [`finish`].
macro_rules! root {
    ($mode:ty, $this:ident, $value:expr, $limits:expr, $resolver:expr;
     $(fn $method:ident($($arg:ident: $ty:ty),*);)*) => {$(
        fn $method<V: Visitor<'de>>(self, $($arg: $ty,)* v: V) -> Result<V::Value, Error> {
            let $this = self;
            let limits = $limits;
            run::<$mode, _>($value, &limits, $resolver, |d| d.$method($($arg,)* v))
        }
    )*};
}

macro_rules! root_methods {
    ($mode:ty, $this:ident, $value:expr, $limits:expr, $resolver:expr) => {
        root! { $mode, $this, $value, $limits, $resolver;
            fn deserialize_any();
            fn deserialize_bool();
            fn deserialize_i8();
            fn deserialize_i16();
            fn deserialize_i32();
            fn deserialize_i64();
            fn deserialize_i128();
            fn deserialize_u8();
            fn deserialize_u16();
            fn deserialize_u32();
            fn deserialize_u64();
            fn deserialize_u128();
            fn deserialize_f32();
            fn deserialize_f64();
            fn deserialize_char();
            fn deserialize_str();
            fn deserialize_string();
            fn deserialize_bytes();
            fn deserialize_byte_buf();
            fn deserialize_option();
            fn deserialize_unit();
            fn deserialize_seq();
            fn deserialize_map();
            fn deserialize_identifier();
            fn deserialize_ignored_any();
            fn deserialize_unit_struct(name: &'static str);
            fn deserialize_tuple(len: usize);
            fn deserialize_tuple_struct(name: &'static str, len: usize);
            fn deserialize_struct(name: &'static str, fields: &'static [&'static str]);
            fn deserialize_enum(name: &'static str, variants: &'static [&'static str]);
        }
    };
}

impl<'de> de::Deserializer<'de> for OwnedRoot {
    type Error = Error;

    root_methods!(
        Copied,
        this,
        &this.value,
        this.limits.clone(),
        this.resolver
    );

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        if name == VALUE_MAGIC {
            // `from_value::<Value>`: the value itself, after the depth check.
            if let Err(e) = self.value.check_depth(&self.limits) {
                return Err(e.into_error().finish(None, Vec::new()));
            }
            return hand_off(self.value, v).map_err(finish);
        }
        let limits = self.limits.clone();
        run::<Copied, _>(&self.value, &limits, self.resolver, |d| {
            d.deserialize_newtype_struct(name, v)
        })
    }
}

/// Reads an owned `Value` as any `T: Deserialize`, like
/// [`from_value`] (default [`Limits`]).
///
/// ```
/// use serde::Deserialize;
///
/// let v: oneyaml::Value = oneyaml::from_str("[1, 2]").unwrap();
/// let pair = <(u8, u8)>::deserialize(v).unwrap();
/// assert_eq!(pair, (1, 2));
/// ```
impl<'de> de::Deserializer<'de> for Value {
    type Error = Error;

    root_methods!(
        Copied,
        this,
        &this,
        Limits::default(),
        Resolver::new(&Settings::new())
    );

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        OwnedRoot {
            value: self,
            limits: Limits::default(),
            resolver: Resolver::new(&Settings::new()),
        }
        .deserialize_newtype_struct(name, v)
    }
}

/// Reads a borrowed `Value` as any `T: Deserialize<'de>`; string fields
/// may borrow from it (`&str`). Default [`Limits`].
///
/// ```
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct Env<'a> {
///     name: &'a str,
/// }
/// let v: oneyaml::Value = oneyaml::from_str("name: LOG_LEVEL").unwrap();
/// let env = Env::deserialize(&v).unwrap();
/// assert_eq!(env.name, "LOG_LEVEL");
/// ```
impl<'de> de::Deserializer<'de> for &'de Value {
    type Error = Error;

    root_methods!(
        Borrowed,
        this,
        this,
        Limits::default(),
        Resolver::new(&Settings::new())
    );

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        run::<Borrowed, _>(
            self,
            &Limits::default(),
            Resolver::new(&Settings::new()),
            |d| d.deserialize_newtype_struct(name, v),
        )
    }
}
