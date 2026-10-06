//! [`Value`]: an owned, span-free YAML value.
//!
//! Mirrors `serde_yaml::Value` where the rules allow: tags are kept, any
//! `Value` can be a mapping key, and `Value` is `Eq` + `Hash`. Every
//! operation that walks nesting (drop, clone, `==`, hash, `Debug`, depth) is
//! iterative, so values of any depth are safe on a small stack.

use std::fmt;
use std::hash::{Hash, Hasher};

use oneyaml_core::Limits;

mod index;
#[cfg(feature = "serde_json")]
mod json;
mod mapping;
mod number;
mod serde_impl;
mod tag;
pub(crate) mod walk;

pub use index::ValueIndex;
pub(crate) use mapping::KeyRef;
pub use mapping::{IntoIter as MappingIntoIter, Iter as MappingIter, Mapping};
pub use number::Number;
pub(crate) use number::{MAX_CONVERTED_DIGITS, Radix};
pub(crate) use serde_impl::{
    VALUE_MAGIC, arm, disarm, hand_off, is_tls_gone, take_handed_over, tls_gone_ser,
};
pub use tag::Tag;
pub(crate) use tag::{TagPrefix, TagStr};

#[cfg(test)]
mod tests;

/// Hash discriminant of `Value::String`, shared with `KeyRef::Str`.
pub(crate) const STRING_TAG: u8 = 3;

/// An owned YAML value.
///
/// Produced only from resolved core data: the same resolver decides
/// the meaning of every scalar for `Value`, serde, and `Document`.
///
/// `Deserialize` builds the value iteratively when used with oneyaml's
/// deserializers, and oneyaml's serializers emit it iteratively (any depth
/// is safe; `Limits::max_depth` bounds it). Through other
/// serializers, `Serialize` refuses values deeper than the default
/// `Limits::max_typed_depth` (`Y2007`).
///
/// # Examples
///
/// The `serde_yaml`-style accessors:
///
/// ```
/// use oneyaml::Value;
///
/// let mut v: Value = oneyaml::from_str(
///     "name: web\nreplicas: 3\nratio: 0.5\ndebug: false\nhost: ~\nports: [80, 443]\ncolor: !rgb ff0000\n",
/// )
/// .unwrap();
/// assert_eq!(v.depth(), 2);
/// assert!(v.is_mapping() && !v.is_sequence());
/// assert!(v["name"].is_string());
/// assert!(v["replicas"].is_number() && v["replicas"].is_i64() && v["replicas"].is_u64());
/// assert_eq!(v["replicas"].as_i64(), Some(3));
/// assert_eq!(v["replicas"].as_number().and_then(|n| n.as_u64()), Some(3));
/// assert!(v["ratio"].is_f64());
/// assert_eq!(v["ratio"].as_f64(), Some(0.5));
/// assert!(v["debug"].is_bool());
/// assert_eq!(v["debug"].as_bool(), Some(false));
/// assert!(v["host"].is_null());
/// assert_eq!(v["host"].as_null(), Some(()));
/// assert!(v["ports"].is_sequence());
/// // A tag the schema does not resolve stays on the value.
/// assert!(v["color"].is_tagged());
/// assert_eq!(v["color"].untag_ref().as_str(), Some("ff0000"));
/// let mut color = v["color"].clone();
/// *color.untag_mut() = Value::from("00ff00"); // keeps the tag
/// assert!(color.is_tagged());
/// assert_eq!(color.untag(), Value::from("00ff00"));
/// // Mutable access to the collections.
/// v.as_mapping_mut().unwrap().insert("tier".into(), "frontend".into());
/// v.get_mut("ports").and_then(|p| p.as_sequence_mut()).unwrap().push(8080.into());
/// assert_eq!(v["ports"][2].as_u64(), Some(8080));
/// assert_eq!(v["tier"].as_str(), Some("frontend"));
/// ```
#[non_exhaustive]
#[derive(Default)]
pub enum Value {
    /// `null`, `~`, an empty node, … (per the active schema).
    #[default]
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer or float.
    Number(Number),
    /// A string.
    String(String),
    /// A sequence.
    Sequence(Sequence),
    /// A mapping with any-`Value` keys, in insertion order.
    Mapping(Mapping),
    /// A value with a tag the schema does not resolve (e.g. `!color red`).
    Tagged(Box<TaggedValue>),
}

/// A YAML sequence of [`Value`]s.
///
/// Every accessor is non-panicking: out-of-range positions
/// give `None` or `Err`, never a panic. There is deliberately no
/// `Deref`/`DerefMut` to `Vec`, whose indexing and `insert`/`remove` panic;
/// [`Sequence::as_slice`], [`Sequence::into_vec`], and
/// `From<Vec<Value>>` are the explicit escape hatches to std types.
///
/// Drop, clone, equality, hash, and `Debug` never recurse.
///
/// ```
/// use oneyaml::{Sequence, Value};
///
/// let mut s: Sequence = vec![Value::from(1), Value::from(2)].into();
/// assert_eq!(s.last(), Some(&Value::from(2)));
/// for item in s.iter_mut() {
///     *item = Value::from(item.as_i64().unwrap_or(0) * 10);
/// }
/// assert_eq!(s.as_slice(), &[Value::from(10), Value::from(20)]);
/// assert_eq!(s.clone().into_vec().len(), 2);
/// s.clear();
/// assert!(s.is_empty());
/// ```
#[derive(Default)]
pub struct Sequence {
    pub(crate) items: Vec<Value>,
}

impl Sequence {
    /// An empty sequence.
    #[must_use]
    pub const fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// The number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when there are no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The item at `index`, or `None` past the end.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.items.get(index)
    }

    /// The item at `index`, mutably, or `None` past the end.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
        self.items.get_mut(index)
    }

    /// The first item.
    #[must_use]
    pub fn first(&self) -> Option<&Value> {
        self.items.first()
    }

    /// The last item.
    #[must_use]
    pub fn last(&self) -> Option<&Value> {
        self.items.last()
    }

    /// Appends `value`.
    pub fn push(&mut self, value: Value) {
        self.items.push(value);
    }

    /// Removes and returns the last item.
    pub fn pop(&mut self) -> Option<Value> {
        self.items.pop()
    }

    /// Inserts `value` at `index` (`0..=len`), shifting later items up.
    ///
    /// # Errors
    ///
    /// `index > len`: the sequence is unchanged and `value` is handed back.
    pub fn insert_at(&mut self, index: usize, value: Value) -> Result<(), Value> {
        if index > self.items.len() {
            return Err(value);
        }
        self.items.insert(index, value);
        Ok(())
    }

    /// Removes and returns the item at `index`, shifting later items down;
    /// `None` past the end.
    pub fn remove_at(&mut self, index: usize) -> Option<Value> {
        (index < self.items.len()).then(|| self.items.remove(index))
    }

    /// Removes every item.
    pub fn clear(&mut self) {
        walk::drop_items(std::mem::take(&mut self.items));
    }

    /// Iterates the items in order.
    pub fn iter(&self) -> std::slice::Iter<'_, Value> {
        self.items.iter()
    }

    /// Iterates the items in order, mutably.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Value> {
        self.items.iter_mut()
    }

    /// The items as a slice (an explicit escape hatch: slice indexing
    /// panics out of range; prefer [`Sequence::get`]).
    #[must_use]
    pub fn as_slice(&self) -> &[Value] {
        &self.items
    }

    /// Takes the items out as a plain `Vec` (an explicit escape hatch).
    #[must_use]
    pub fn into_vec(mut self) -> Vec<Value> {
        std::mem::take(&mut self.items)
    }
}

impl Extend<Value> for Sequence {
    fn extend<I: IntoIterator<Item = Value>>(&mut self, iter: I) {
        self.items.extend(iter);
    }
}

impl<'a> IntoIterator for &'a mut Sequence {
    type Item = &'a mut Value;
    type IntoIter = std::slice::IterMut<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter_mut()
    }
}

impl From<Vec<Value>> for Sequence {
    fn from(items: Vec<Value>) -> Self {
        Self { items }
    }
}

impl FromIterator<Value> for Sequence {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        Self {
            items: iter.into_iter().collect(),
        }
    }
}

impl IntoIterator for Sequence {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a Sequence {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl Drop for Sequence {
    // Iterative teardown; the derived drop glue recurses per level.
    fn drop(&mut self) {
        walk::drop_items(std::mem::take(&mut self.items));
    }
}

impl Clone for Sequence {
    fn clone(&self) -> Self {
        Self {
            items: self.items.iter().map(walk::clone_value).collect(),
        }
    }
}

impl PartialEq for Sequence {
    fn eq(&self, other: &Self) -> bool {
        walk::eq_sequences(self, other)
    }
}

impl Eq for Sequence {}

impl Hash for Sequence {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(walk::digest_sequence(self));
    }
}

impl fmt::Debug for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        walk::debug_sequence(self, f)
    }
}

/// A value with a tag the schema does not resolve.
pub struct TaggedValue {
    /// The tag.
    pub tag: Tag,
    /// The tagged content. For a scalar this is always a string: a custom
    /// tag gives the schema no licence to read `!x 12` as a number.
    pub value: Value,
}

impl TaggedValue {
    /// Splits into tag and content.
    #[must_use]
    pub fn into_parts(mut self) -> (Tag, Value) {
        let tag = std::mem::replace(&mut self.tag, Tag::new(String::new()));
        (tag, std::mem::replace(&mut self.value, Value::Null))
    }
}

impl Drop for TaggedValue {
    // A chain of tags must not drop recursively either.
    fn drop(&mut self) {
        walk::drop_value(std::mem::replace(&mut self.value, Value::Null));
    }
}

impl Clone for TaggedValue {
    fn clone(&self) -> Self {
        Self {
            tag: self.tag.clone(),
            value: walk::clone_value(&self.value),
        }
    }
}

impl PartialEq for TaggedValue {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag && walk::eq(&self.value, &other.value)
    }
}

impl Eq for TaggedValue {}

impl Hash for TaggedValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.tag.hash(state);
        state.write_u64(walk::digest(&self.value));
    }
}

impl fmt::Debug for TaggedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TaggedValue({}, ", self.tag)?;
        walk::debug_value(&self.value, f)?;
        f.write_str(")")
    }
}

// Hand-written, iterative; derived impls recurse once per level.
impl Clone for Value {
    fn clone(&self) -> Self {
        walk::clone_value(self)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        walk::eq(self, other)
    }
}

impl Eq for Value {}

impl Hash for Value {
    /// One `u64` structural digest, consistent with `==` (mapping entry
    /// order does not matter), computed iteratively.
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(walk::digest(self));
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        walk::debug_value(self, f)
    }
}

/// A [`Value`] nests deeper than [`Limits::get_max_depth`] allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DepthExceeded {
    /// The configured limit.
    pub(crate) limit: usize,
}

impl DepthExceeded {
    /// The `Y2005` limit diagnostic, naming the setting to raise.
    pub(crate) fn into_error(self) -> crate::Error {
        crate::Error::new(
            oneyaml_core::code::VALUE_DEPTH_EXCEEDED,
            format!("Value nests deeper than {} levels", self.limit),
            None,
        )
        .with_help(format!(
            "raise Limits::max_depth (currently {})",
            self.limit
        ))
    }
}

impl Value {
    /// The collection nesting depth: 0 for a scalar, 1 for a collection of
    /// scalars, and so on. Tags add nothing. Iterative.
    #[must_use]
    pub fn depth(&self) -> usize {
        walk::depth(self, usize::MAX)
    }

    /// `max_depth` applies wherever our APIs consume a `Value` (the
    /// serializer, emitter, `Document` insertion). Stops walking as soon as
    /// the limit is exceeded. Consumers map the error to the limit
    /// diagnostic.
    pub(crate) fn check_depth(&self, limits: &Limits) -> Result<(), DepthExceeded> {
        let limit = limits.get_max_depth();
        if walk::depth(self, limit) > limit {
            Err(DepthExceeded { limit })
        } else {
            Ok(())
        }
    }

    /// True for `Null`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// `Some(())` for `Null` (`serde_yaml` shape).
    #[must_use]
    pub fn as_null(&self) -> Option<()> {
        self.is_null().then_some(())
    }

    /// True for `Bool`.
    #[must_use]
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }

    /// The boolean, if this is a `Bool`.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// True for `Number`.
    #[must_use]
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }

    /// The number, if this is a `Number`.
    #[must_use]
    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// True for an integer that fits `i64`.
    #[must_use]
    pub fn is_i64(&self) -> bool {
        self.as_i64().is_some()
    }

    /// The integer as `i64`, if it is one and fits.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number().and_then(Number::as_i64)
    }

    /// True for an integer that fits `u64`.
    #[must_use]
    pub fn is_u64(&self) -> bool {
        self.as_u64().is_some()
    }

    /// The integer as `u64`, if it is one and fits.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.as_number().and_then(Number::as_u64)
    }

    /// True for a float.
    #[must_use]
    pub fn is_f64(&self) -> bool {
        self.as_number().is_some_and(Number::is_float)
    }

    /// The number as `f64` (see [`Number::as_f64`]).
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number().and_then(Number::as_f64)
    }

    /// True for `String`.
    #[must_use]
    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// The string, if this is a `String`.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// True for `Sequence`.
    #[must_use]
    pub fn is_sequence(&self) -> bool {
        matches!(self, Value::Sequence(_))
    }

    /// The sequence, if this is a `Sequence`.
    #[must_use]
    pub fn as_sequence(&self) -> Option<&Sequence> {
        match self {
            Value::Sequence(s) => Some(s),
            _ => None,
        }
    }

    /// The sequence, mutably, if this is a `Sequence`.
    pub fn as_sequence_mut(&mut self) -> Option<&mut Sequence> {
        match self {
            Value::Sequence(s) => Some(s),
            _ => None,
        }
    }

    /// True for `Mapping`.
    #[must_use]
    pub fn is_mapping(&self) -> bool {
        matches!(self, Value::Mapping(_))
    }

    /// The mapping, if this is a `Mapping`.
    #[must_use]
    pub fn as_mapping(&self) -> Option<&Mapping> {
        match self {
            Value::Mapping(m) => Some(m),
            _ => None,
        }
    }

    /// The mapping, mutably, if this is a `Mapping`.
    pub fn as_mapping_mut(&mut self) -> Option<&mut Mapping> {
        match self {
            Value::Mapping(m) => Some(m),
            _ => None,
        }
    }

    /// True for `Tagged`.
    #[must_use]
    pub fn is_tagged(&self) -> bool {
        matches!(self, Value::Tagged(_))
    }

    /// The tagged value, if this is `Tagged`.
    #[must_use]
    pub fn as_tagged(&self) -> Option<&TaggedValue> {
        match self {
            Value::Tagged(t) => Some(t),
            _ => None,
        }
    }

    /// The content under any number of tags (iterative).
    #[must_use]
    pub fn untag_ref(&self) -> &Value {
        let mut v = self;
        while let Value::Tagged(t) = v {
            v = &t.value;
        }
        v
    }

    /// The content under any number of tags, mutably (iterative).
    pub fn untag_mut(&mut self) -> &mut Value {
        let mut v = self;
        while let Value::Tagged(t) = v {
            v = &mut t.value;
        }
        v
    }

    /// Removes all tags, returning the content (iterative).
    #[must_use]
    pub fn untag(self) -> Value {
        let mut v = self;
        while let Value::Tagged(t) = v {
            v = t.into_parts().1;
        }
        v
    }

    /// Looks up `index` (a `usize` for sequences, a string or `Value` key
    /// for mappings), looking through tags. Never panics.
    #[must_use]
    pub fn get<I: ValueIndex>(&self, index: I) -> Option<&Value> {
        index.index_into(self)
    }

    /// Like [`Value::get`], mutably.
    pub fn get_mut<I: ValueIndex>(&mut self, index: I) -> Option<&mut Value> {
        index.index_into_mut(self)
    }
}

macro_rules! from_number {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Value::Number(Number::from(v))
            }
        }
    )*};
}
from_number!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, f32, f64
);

impl From<Number> for Value {
    fn from(v: Number) -> Self {
        Value::Number(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_owned())
    }
}

impl From<Sequence> for Value {
    fn from(v: Sequence) -> Self {
        Value::Sequence(v)
    }
}

impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::Sequence(Sequence::from(v))
    }
}

impl From<Mapping> for Value {
    fn from(v: Mapping) -> Self {
        Value::Mapping(v)
    }
}

impl From<TaggedValue> for Value {
    fn from(v: TaggedValue) -> Self {
        Value::Tagged(Box::new(v))
    }
}

impl FromIterator<Value> for Value {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        Value::Sequence(iter.into_iter().collect())
    }
}
