//! [`Spanned<T>`]: a value together with the source span of its node
//! (for error messages and tooling).

use std::fmt;
use std::ops::{Deref, DerefMut};

use oneyaml_core::Span;
use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, Visitor};

use crate::error::Error;

const NAME: &str = "$__oneyaml_private_Spanned";
const START: &str = "$__oneyaml_private_start";
const END: &str = "$__oneyaml_private_end";
const VALUE: &str = "$__oneyaml_private_value";
const FIELDS: &[&str] = &[START, END, VALUE];

/// Whether a `deserialize_struct` call is [`Spanned`]'s.
pub(crate) fn is_spanned(name: &str, fields: &[&str]) -> bool {
    name == NAME && fields == FIELDS
}

/// A deserialized value with the UTF-8 byte span of the node it came from.
///
/// Only oneyaml's deserializers know spans; other formats fail to
/// deserialize a `Spanned<T>`. A node reached through an alias spans the
/// alias itself (`*name`, start and end), whether it is a scalar, a
/// sequence, or a mapping, as error positions do; nodes *inside* an
/// aliased collection keep the spans of their anchored source. Every span
/// is well-formed: `start <= end <= src.len()`.
///
/// ```
/// #[derive(serde::Deserialize)]
/// struct Config {
///     port: oneyaml::Spanned<u16>,
/// }
/// let c: Config = oneyaml::from_str("port: 8080\n").unwrap();
/// assert_eq!(*c.port, 8080);
/// assert_eq!((c.port.span().start, c.port.span().end), (6, 10));
/// assert_eq!(c.port.get_ref(), &8080);
/// ```
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Spanned<T> {
    span: Span,
    value: T,
}

impl<T> Spanned<T> {
    /// A value with a span.
    #[must_use]
    pub const fn new(span: Span, value: T) -> Self {
        Self { span, value }
    }

    /// The node's source span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The value.
    #[must_use]
    pub const fn get_ref(&self) -> &T {
        &self.value
    }

    /// The value, mutably.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.value
    }

    /// The value, without its span.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T> Deref for Spanned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for Spanned<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T: fmt::Debug> fmt::Debug for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Spanned({}..{}, ", self.span.start, self.span.end)?;
        self.value.fmt(f)?;
        f.write_str(")")
    }
}

impl<T: serde::Serialize> serde::Serialize for Spanned<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(s)
    }
}

impl<'de, T: de::Deserialize<'de>> de::Deserialize<'de> for Spanned<T> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_struct(NAME, FIELDS, SpannedVisitor(std::marker::PhantomData))
    }
}

struct SpannedVisitor<T>(std::marker::PhantomData<T>);

impl<'de, T: de::Deserialize<'de>> Visitor<'de> for SpannedVisitor<T> {
    type Value = Spanned<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a spanned value (only oneyaml's deserializers provide spans)")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let (mut start, mut end, mut value) = (None, None, None);
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                START => start = Some(map.next_value::<usize>()?),
                END => end = Some(map.next_value::<usize>()?),
                VALUE => value = Some(map.next_value::<T>()?),
                other => return Err(de::Error::unknown_field(other, FIELDS)),
            }
        }
        match (start, end, value) {
            (Some(start), Some(end), Some(value)) => Ok(Spanned::new(Span::new(start, end), value)),
            (None, _, _) => Err(de::Error::missing_field(START)),
            (_, None, _) => Err(de::Error::missing_field(END)),
            (_, _, None) => Err(de::Error::missing_field(VALUE)),
        }
    }
}

/// The map a oneyaml deserializer shows `Spanned`: `start`, then `value`
/// (deserialized from `de`), then `end`.
pub(crate) struct SpannedAccess<D> {
    start: usize,
    end: usize,
    de: Option<D>,
    step: u8,
}

impl<D> SpannedAccess<D> {
    /// `end` is used as is for scalars; for collections the caller passes a
    /// deserializer whose end is read after the value (see `Core`).
    pub(crate) fn new(start: usize, end: usize, de: D) -> Self {
        Self {
            start,
            end,
            de: Some(de),
            step: 0,
        }
    }
}

impl<'de, D: EndAfter<'de>> MapAccess<'de> for SpannedAccess<D> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let key = match self.step {
            0 => START,
            1 => VALUE,
            2 => END,
            _ => return Ok(None),
        };
        seed.deserialize(de::value::BorrowedStrDeserializer::new(key))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let step = self.step;
        self.step = step.saturating_add(1);
        match step {
            0 => seed.deserialize(self.start.into_deserializer()),
            1 => match self.de.take() {
                Some(d) => {
                    let (value, end) = d.value_and_end(seed)?;
                    if let Some(end) = end {
                        self.end = end;
                    }
                    Ok(value)
                }
                None => Err(Error::internal("spanned value read twice", None)),
            },
            _ => seed.deserialize(self.end.into_deserializer()),
        }
    }
}

/// A deserializer that can report where its node ended after
/// deserializing it (`None`: the span given up front stands).
pub(crate) trait EndAfter<'de>: de::Deserializer<'de, Error = Error> {
    fn value_and_end<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Option<usize>), Error>;
}

impl<'de> EndAfter<'de> for crate::de::scalar::ScalarDe<'de> {
    fn value_and_end<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Option<usize>), Error> {
        seed.deserialize(self).map(|v| (v, None))
    }
}
