//! serde's data model → [`Value`].
//!
//! [`ToValue`] is a `serde::Serializer` that builds a `Value`: for
//! `to_value`, and for map keys in the streaming serializer
//! ([`super::stream`]), whose output is defined as this build
//! emitted (the enum forms below included). serde's protocol recurses
//! once per level of the user's type, so every nested call (a
//! collection, an element, a field, `Some`, a newtype) counts one level
//! against `Limits::max_typed_depth`; one level more is `Y2007`, never a
//! stack overflow. A `Value` inside
//! the data is not walked through serde at all: it is handed over whole
//! ([`build`] marks each nested call as ours; see `crate::value::arm`) and
//! emitted iteratively, bounded by `Limits::max_depth` only.
//!
//! # Enums (matching `serde_yaml` where our reader allows)
//!
//! * unit variant → the variant name as a string (`Unit`);
//! * newtype, tuple, and struct variants → a tagged node, `!Name content`,
//!   when the content reads back through a tag: a string, a sequence, or a
//!   mapping;
//! * otherwise (a newtype variant around a number, bool, null, or another
//!   variant) → a one-entry mapping `Name: content`: a custom-tagged scalar
//!   always reads back as a string, so `!Name 1` would not read back
//!   as the number. The deserializer reads both forms. (The streaming
//!   serializer writes `!Name 1` under `Layout::SerdeYaml` read back with
//!   `Settings::resolve_tagged_variant_content`; a `Value` keeps
//!   the map form.)

use serde::ser::{self, Serialize};

use oneyaml_core::code;

use crate::error::Error;
use crate::value::{
    Mapping, Number, Sequence, Tag, TaggedValue, VALUE_MAGIC, Value, arm, disarm, take_handed_over,
    variant_tag_text,
};

/// Builds a [`Value`] from serde calls, `depth` levels deep.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ToValue {
    depth: usize,
    max: usize,
}

impl ToValue {
    /// The top level, with `Limits::max_typed_depth` = `max`.
    pub(crate) fn root(max: usize) -> Self {
        Self { depth: 0, max }
    }

    /// One level deeper; `Y2007` past the limit.
    pub(crate) fn nested(self) -> Result<Self, Error> {
        let depth = self.depth.saturating_add(1);
        if depth > self.max {
            return Err(Error::new(
                code::TYPED_DEPTH_EXCEEDED,
                format!(
                    "the value being serialized nests deeper than {} levels",
                    self.max
                ),
                None,
            )
            .with_help(format!(
                "raise Limits::max_typed_depth (currently {}); serializing a Rust type recurses \
                 once per level, so the thread's stack must hold that many levels",
                self.max
            )));
        }
        Ok(Self {
            depth,
            max: self.max,
        })
    }
}

/// Serializes `v` into oneyaml's builder `s`, with the mark that tells a
/// `Value` inside it is ours (only then the private
/// hand-over; see `crate::value::arm`). The mark found on entry is put
/// back on exit, so a `Serialize` impl that calls oneyaml re-entrantly
/// keeps its caller's mark.
pub(crate) fn build<T: ?Sized + Serialize>(v: &T, s: ToValue) -> Result<Value, Error> {
    let _armed = arm();
    v.serialize(s)
}

/// Whether a handed-over `Value` is a scalar leaf (no nesting below it).
pub(crate) fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

/// A variant with content (see the module docs). Any non-empty name can
/// be a local tag: characters a tag cannot hold are `%`-escaped when
/// written, and the reader decodes them back (as `serde_yaml` does).
fn variant(name: &str, content: Value) -> Value {
    match content {
        Value::String(_) | Value::Sequence(_) | Value::Mapping(_) if !name.is_empty() => {
            Value::Tagged(Box::new(TaggedValue {
                tag: Tag::new(variant_tag_text(name)),
                value: content,
            }))
        }
        other => {
            let mut m = Mapping::new();
            m.insert(Value::from(name), other);
            Value::Mapping(m)
        }
    }
}

/// An `f32` as the `f64` of its shortest text, so it is written with the
/// `f32`'s own shortest digits (`1.1`, not `1.100000023841858`) and reads
/// back as the same `f32`. Formatted into a stack buffer (no
/// allocation per `f32`); `{:e}` is the shortest
/// round-trip form, at most 15 bytes for an `f32` (`-1.00211145e-36`).
pub(crate) fn f32_value(v: f32) -> f64 {
    use std::fmt::Write as _;
    if !v.is_finite() {
        return f64::from(v);
    }
    let mut buf = StackText::default();
    if write!(buf, "{v:e}").is_err() {
        return f64::from(v);
    }
    buf.as_str()
        .and_then(|t| t.parse::<f64>().ok())
        .unwrap_or(f64::from(v))
}

/// `Y3014`: a mapping being serialized has `key` twice (the build and the
/// streaming serializer).
pub(crate) fn duplicate_key(key: Option<&str>) -> Error {
    let e = Error::new(
        code::SERIALIZE_DUPLICATE_KEY,
        "the mapping being serialized has this key twice",
        None,
    )
    .with_help("YAML 1.2 requires unique keys; e.g. rename a field that a flattened map repeats");
    match key {
        Some(n) => e.in_key(n),
        None => e,
    }
}

/// `Y3016`: `serialize_key` twice, or `end` after a key.
pub(crate) fn key_without_value() -> Error {
    Error::new(
        code::INVALID_SERIALIZE_CALLS,
        "a map key was serialized without its value",
        None,
    )
}

/// `Y3016`: `serialize_value` with no key before it.
pub(crate) fn value_without_key() -> Error {
    Error::new(
        code::INVALID_SERIALIZE_CALLS,
        "a map value was serialized without its key",
        None,
    )
}

/// A small fixed-capacity text buffer; a write that does not fit fails.
#[derive(Default)]
struct StackText {
    bytes: [u8; 32],
    len: usize,
}

impl StackText {
    fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.bytes.get(..self.len)?).ok()
    }
}

impl std::fmt::Write for StackText {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(std::fmt::Error)?;
        let dst = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        dst.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// The compound builder behind every `Serialize*` trait.
pub(crate) struct Compound {
    /// Serializer for the children (one level deeper).
    child: ToValue,
    kind: Kind,
    items: Vec<Value>,
    map: Mapping,
    key: Option<Value>,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Seq,
    Map,
    TupleVariant(&'static str),
    StructVariant(&'static str),
}

impl Compound {
    fn new(parent: ToValue, kind: Kind, len: Option<usize>) -> Result<Self, Error> {
        let items = match kind {
            // A size hint, bounded so a lying `len` cannot allocate much.
            Kind::Seq | Kind::TupleVariant(_) => Vec::with_capacity(len.unwrap_or(0).min(4096)),
            Kind::Map | Kind::StructVariant(_) => Vec::new(),
        };
        Ok(Self {
            child: parent.nested()?,
            kind,
            items,
            map: Mapping::new(),
            key: None,
        })
    }

    fn element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        let index = self.items.len();
        let v = build(v, self.child).map_err(|e| e.in_index(index))?;
        self.items.push(v);
        Ok(())
    }

    fn insert(&mut self, k: Value, v: Value) -> Result<(), Error> {
        // The key's name is read in the error path only, never
        // copied on the success path.
        if self.map.contains_key(&k) {
            return Err(duplicate_key(k.as_str()));
        }
        self.map.insert(k, v);
        Ok(())
    }

    fn value_for_key<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        let Some(k) = self.key.take() else {
            return Err(value_without_key());
        };
        let v = match build(v, self.child) {
            Ok(v) => v,
            Err(e) => {
                return Err(match k.as_str() {
                    Some(n) => e.in_key(n),
                    None => e,
                });
            }
        };
        self.insert(k, v)
    }

    fn field<T: ?Sized + Serialize>(&mut self, name: &'static str, v: &T) -> Result<(), Error> {
        let v = build(v, self.child).map_err(|e| e.in_key(name))?;
        self.insert(Value::from(name), v)
    }

    fn finish(self) -> Result<Value, Error> {
        if self.key.is_some() {
            return Err(key_without_value());
        }
        Ok(match self.kind {
            Kind::Seq => Value::Sequence(Sequence::from(self.items)),
            Kind::Map => Value::Mapping(self.map),
            Kind::TupleVariant(name) => variant(name, Value::Sequence(Sequence::from(self.items))),
            Kind::StructVariant(name) => variant(name, Value::Mapping(self.map)),
        })
    }
}

impl ser::SerializeSeq for Compound {
    type Ok = Value;
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound {
    type Ok = Value;
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.element(v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

impl ser::SerializeMap for Compound {
    type Ok = Value;
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), Error> {
        if self.key.is_some() {
            return Err(key_without_value());
        }
        self.key = Some(build(k, self.child)?);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.value_for_key(v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        name: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.field(name, v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        name: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.field(name, v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

impl ser::Serializer for ToValue {
    type Ok = Value;
    type Error = Error;
    type SerializeSeq = Compound;
    type SerializeTuple = Compound;
    type SerializeTupleStruct = Compound;
    type SerializeTupleVariant = Compound;
    type SerializeMap = Compound;
    type SerializeStruct = Compound;
    type SerializeStructVariant = Compound;

    fn serialize_bool(self, v: bool) -> Result<Value, Error> {
        disarm();
        Ok(Value::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_i128(self, v: i128) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_u8(self, v: u8) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_u128(self, v: u128) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_f32(self, v: f32) -> Result<Value, Error> {
        disarm();
        Ok(Value::Number(Number::from(f32_value(v))))
    }
    fn serialize_f64(self, v: f64) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    fn serialize_char(self, v: char) -> Result<Value, Error> {
        disarm();
        Ok(Value::String(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(v))
    }
    /// A sequence of byte values (as `serde_yaml`).
    fn serialize_bytes(self, v: &[u8]) -> Result<Value, Error> {
        disarm();
        self.nested()?;
        Ok(v.iter().map(|b| Value::from(*b)).collect())
    }
    fn serialize_none(self) -> Result<Value, Error> {
        disarm();
        Ok(Value::Null)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<Value, Error> {
        disarm();
        build(v, self.nested()?)
    }
    fn serialize_unit(self) -> Result<Value, Error> {
        disarm();
        Ok(Value::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Value, Error> {
        disarm();
        Ok(Value::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
    ) -> Result<Value, Error> {
        disarm();
        Ok(Value::from(name))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        v: &T,
    ) -> Result<Value, Error> {
        disarm();
        if name == VALUE_MAGIC {
            // A `Value` is taken whole and emitted iteratively.
            if let Some(v) = take_handed_over() {
                // A handed-over scalar is a
                // leaf, like a `u64` leaf, so it costs no typed level;
                // collections (and tags, which wrap one) keep counting.
                if !is_scalar(&v) {
                    self.nested()?;
                }
                return Ok(v);
            }
            // A user type spoofing the private name finds nothing
            // handed over; its content runs with the mark cleared (plain
            // serde data throughout), never with a hand-over pending.
            return v.serialize(self.nested()?);
        }
        build(v, self.nested()?)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        v: &T,
    ) -> Result<Value, Error> {
        disarm();
        let content = build(v, self.nested()?).map_err(|e| e.in_key(name))?;
        Ok(variant(name, content))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Compound, Error> {
        disarm();
        Compound::new(self, Kind::Seq, len)
    }
    fn serialize_tuple(self, len: usize) -> Result<Compound, Error> {
        disarm();
        Compound::new(self, Kind::Seq, Some(len))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<Compound, Error> {
        disarm();
        Compound::new(self, Kind::Seq, Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        len: usize,
    ) -> Result<Compound, Error> {
        disarm();
        Compound::new(self, Kind::TupleVariant(name), Some(len))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound, Error> {
        disarm();
        Compound::new(self, Kind::Map, None)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Compound, Error> {
        disarm();
        Compound::new(self, Kind::Map, None)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        _: usize,
    ) -> Result<Compound, Error> {
        disarm();
        Compound::new(self, Kind::StructVariant(name), None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use serde::ser::{SerializeMap, Serializer as _};

    #[test]
    fn map_protocol_errors() {
        let mut m = ToValue::root(8).serialize_map(None).unwrap();
        assert_eq!(
            m.serialize_value(&1).unwrap_err().code(),
            code::INVALID_SERIALIZE_CALLS
        );
        m.serialize_key("a").unwrap();
        assert_eq!(
            m.serialize_key("b").unwrap_err().code(),
            code::INVALID_SERIALIZE_CALLS
        );
        let mut m = ToValue::root(8).serialize_map(None).unwrap();
        m.serialize_key("a").unwrap();
        assert_eq!(
            SerializeMap::end(m).unwrap_err().code(),
            code::INVALID_SERIALIZE_CALLS
        );
    }

    #[test]
    fn enum_forms() {
        assert_eq!(
            variant("A", Value::from("x"))
                .as_tagged()
                .unwrap()
                .tag
                .to_string(),
            "!A"
        );
        assert!(variant("A", Value::from(1)).is_mapping());
        assert_eq!(
            variant("a b", Value::from("x"))
                .as_tagged()
                .unwrap()
                .tag
                .to_string(),
            "!a b"
        );
        assert!(variant("", Value::from("x")).is_mapping());
        assert_eq!(f32_value(1.1), 1.1);
        assert!(f32_value(f32::NAN).is_nan());
        assert_eq!(f32_value(f32::INFINITY), f64::INFINITY);
        // The longest shortest forms fit the buffer and keep their digits.
        for v in [
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::MAX,
            f32::MIN,
            1e-45,
            -1e-45,
            0.1,
        ] {
            assert_eq!(f32_value(v), v.to_string().parse::<f64>().unwrap(), "{v}");
            assert_eq!(f32_value(v) as f32, v);
        }
        assert_eq!(f32_value(-0.0).to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn stack_text_refuses_what_does_not_fit() {
        use std::fmt::Write as _;
        let mut b = StackText::default();
        assert!(b.write_str(&"x".repeat(32)).is_ok());
        assert_eq!(b.as_str(), Some("x".repeat(32).as_str()));
        assert!(b.write_str("y").is_err());
    }
}
