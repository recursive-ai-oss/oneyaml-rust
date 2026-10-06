//! `Serialize` and `Deserialize` for [`Value`].
//!
//! **Deserialize.** `Value` asks for a newtype struct with a private name.
//! oneyaml's deserializers recognise it, build the value with the shared,
//! iterative builder, and hand it over through a thread-local slot
//! ([`hand_off`]): the recursive visitor protocol is never used, so any
//! depth is safe. Other deserializers (e.g. `serde_json`) fall back
//! to the ordinary visitor, whose recursion is bounded by that format's own
//! limits.
//!
//! **Serialize.** Third-party serializers see **plain serde data**:
//! a sequence, a map, a scalar, and a tagged value as
//! the one-entry map `{tag: value}` — never a private wrapper. serde's
//! `Serializer` protocol is recursive by design, so for those the depth is
//! checked first, iteratively, against the default
//! `Limits::max_typed_depth` (128: the same native-stack bound as
//! typed deserialization); only then does serialization recurse, at most
//! that deep. Deeper values fail with `Y2007`.
//!
//! Only oneyaml's own builders (`ToValue`, and the streaming serializer
//! which arms the same way) get something else: each
//! [`arm`]s a thread-local mark right before serializing a child (and puts
//! back the mark it found once that child is done), and a
//! `Value`/`Sequence`/`Mapping`/`Number` that finds the mark (or is serialized
//! straight into the document `Serializer`) hands a clone over through a
//! thread-local slot inside a newtype struct with the private name; the
//! builder takes it ([`take_handed_over`]) for the iterative emitter,
//! bounded by `Limits::max_depth` only. The mark is spent by
//! the first `Value` that reads it and cleared by every builder method, so
//! nothing runs with it set except the user's own `Serialize` code before
//! it calls back into the builder; a newtype that spoofs the private name
//! finds no handed-over value and serializes normally. The clone is
//! the price of the generic `serde::Serialize` path; `Value::to_yaml_string`
//! emits a `&Value` without it.

use std::cell::Cell;
use std::fmt;

use oneyaml_core::{Limits, code};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{self, SerializeMap, SerializeSeq};

use super::{Mapping, Number, Sequence, Tag, TaggedValue, Value};

/// The newtype name `Value` asks for (private protocol).
pub(crate) const VALUE_MAGIC: &str = "$__oneyaml_private_Value";

thread_local! {
    static SLOT: Cell<Option<Value>> = const { Cell::new(None) };
    /// Set by oneyaml's `ToValue` right before it serializes a child
    /// ([`arm`]); taken by the first `Value`/`Sequence`/`Mapping`/`Number` to
    /// serialize, and cleared by every `ToValue` method ([`disarm`]).
    static OURS: Cell<bool> = const { Cell::new(false) };
    /// The clone a `Value` hands to `ToValue` ([`take_handed_over`]), set
    /// only for the duration of one `serialize_newtype_struct` call.
    static SER_SLOT: Cell<Option<Value>> = const { Cell::new(None) };
}

/// Clears [`SLOT`] when dropped, so a value is never left behind: not when
/// the visitor was not `Value`'s (impossible with the private name), and
/// not when it unwinds from a panic between `set` and `take`.
struct SlotGuard;

impl Drop for SlotGuard {
    fn drop(&mut self) {
        // `try_with`: the slot may already be gone during thread teardown.
        if let Ok(Some(left)) = SLOT.try_with(Cell::take) {
            super::walk::drop_value(left);
        }
    }
}

/// Hands a fully built `value` to `Value`'s visitor.
///
/// `Y3017` when the slot is already destroyed (a thread-local destructor
/// deserializing a `Value` during thread teardown): without the slot
/// the value cannot be handed over, and the visitor would otherwise see a
/// unit and build a wrong `Value`.
pub(crate) fn hand_off<'de, V: Visitor<'de>>(value: Value, v: V) -> Result<V::Value, crate::Error> {
    let _guard = SlotGuard;
    if SLOT.try_with(|s| s.set(Some(value))).is_err() {
        return Err(tls_gone());
    }
    v.visit_newtype_struct(de::value::UnitDeserializer::<crate::Error>::new())
}

/// The `Y3017` error of [`hand_off`].
fn tls_gone() -> crate::Error {
    crate::Error::new(
        code::THREAD_LOCAL_UNAVAILABLE,
        "a Value cannot be deserialized while this thread's thread-local storage is being \
         destroyed",
        None,
    )
    .with_help(
        "deserialize into a typed target instead, or deserialize the Value before the thread \
         starts shutting down",
    )
}

/// The `Y3017` error of [`hand_over`], raised through the generic
/// `ser::Error::custom` (`S::Error` is `crate::Error` there, since `S` is
/// ours). `crate::Error`'s `custom` recognises this private type
/// ([`is_tls_gone`]) and gives it its own code; any other serializer would
/// only see its message (it never gets here: `hand_over` is ours only).
pub(crate) struct TlsGone;

impl fmt::Display for TlsGone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "a Value cannot be serialized by oneyaml while this thread's thread-local storage \
             is being destroyed",
        )
    }
}

/// Whether `T` (the message type of a `ser::Error::custom` call) is
/// [`TlsGone`]. `type_name` is not unique across copies of oneyaml in one
/// build graph, but this is a private type: a false positive needs a type
/// with the same path, which only another copy of oneyaml has (its
/// `TlsGone` means the same teardown refusal, so `Y3017` is still right).
pub(crate) fn is_tls_gone<T: ?Sized>() -> bool {
    std::any::type_name::<T>() == std::any::type_name::<TlsGone>()
}

/// The `Y3017` error for [`TlsGone`] (serialize side).
pub(crate) fn tls_gone_ser() -> crate::Error {
    crate::Error::new(code::THREAD_LOCAL_UNAVAILABLE, TlsGone.to_string(), None).with_help(
        "serialize the Value before the thread starts shutting down; third-party serializers \
         still receive it as plain serde data",
    )
}

impl<'de> de::Deserialize<'de> for Value {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_newtype_struct(VALUE_MAGIC, ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_newtype_struct<D: de::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        // `try_with`: during thread teardown the slot may be gone; then
        // nothing was handed off (see `hand_off`), and `d` is a
        // third-party deserializer to read the ordinary way.
        match SLOT.try_with(Cell::take).ok().flatten() {
            Some(v) => Ok(v),
            None => d.deserialize_any(self),
        }
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_i128<E>(self, v: i128) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_u128<E>(self, v: u128) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        de::Deserialize::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(a.size_hint().unwrap_or(0).min(4096));
        while let Some(v) = a.next_element::<Value>()? {
            items.push(v);
        }
        Ok(Value::Sequence(Sequence::from(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
        let mut m = Mapping::new();
        while let Some((k, v)) = a.next_entry::<Value, Value>()? {
            m.insert(k, v);
        }
        Ok(Value::Mapping(m))
    }

    fn visit_enum<A: de::EnumAccess<'de>>(self, a: A) -> Result<Value, A::Error> {
        use de::VariantAccess;
        let (name, variant) = a.variant::<String>()?;
        let value = variant.newtype_variant::<Value>()?;
        Ok(Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new(format!("!{name}")),
            value,
        })))
    }
}

impl<'de> de::Deserialize<'de> for Mapping {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Mapping(m) => Ok(m),
            other => Err(de::Error::invalid_type(unexpected(&other), &"a mapping")),
        }
    }
}

impl<'de> de::Deserialize<'de> for Sequence {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Sequence(s) => Ok(s),
            other => Err(de::Error::invalid_type(unexpected(&other), &"a sequence")),
        }
    }
}

impl<'de> de::Deserialize<'de> for Number {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Number(n) => Ok(n),
            other => Err(de::Error::invalid_type(unexpected(&other), &"a number")),
        }
    }
}

fn unexpected(v: &Value) -> de::Unexpected<'_> {
    match v {
        Value::Null => de::Unexpected::Unit,
        Value::Bool(b) => de::Unexpected::Bool(*b),
        Value::Number(_) => de::Unexpected::Other("number"),
        Value::String(s) => de::Unexpected::Str(s),
        Value::Sequence(_) => de::Unexpected::Seq,
        Value::Mapping(_) => de::Unexpected::Map,
        Value::Tagged(_) => de::Unexpected::Other("tagged value"),
    }
}

/// The deepest `Value` serialized through serde: the default
/// `Limits::max_typed_depth`, since a third-party serializer recurses once
/// per level like a typed deserializer does.
fn serialize_limit() -> usize {
    Limits::default().get_max_typed_depth()
}

/// The `Y2007` error for a value deeper than [`serialize_limit`].
fn too_deep<E: ser::Error>() -> E {
    E::custom(format!(
        "{}: Value nests deeper than {} levels, the limit for serializing through serde \
         (the default Limits::max_typed_depth); oneyaml's own emitter is iterative",
        code::TYPED_DEPTH_EXCEEDED,
        serialize_limit()
    ))
}

/// Depth-checked entry: `Y2007` beyond [`serialize_limit`]. The
/// depth counts every level serde recurses through, tags included
/// ([`super::walk::serde_depth`]), so a chain of tags cannot overflow the
/// stack. Checked first, iteratively, so a value that is too deep
/// never starts recursing (a small stack is enough to be refused).
fn checked<S: ser::Serializer>(v: &Value, s: S) -> Result<S::Ok, S::Error> {
    let limit = serialize_limit();
    if super::walk::serde_depth(v, limit) > limit {
        return Err(too_deep());
    }
    plain(v, s)
}

/// A child being serialized, its depth already checked by the entry.
///
/// A child that reaches oneyaml's own builder, armed, is handed over whole
/// like a `Value` would be: that happens when a `Mapping` is serialized
/// through serde's `#[serde(flatten)]` wrapper (a serializer that is not
/// ours, so the mapping takes this plain path) and its entries land in
/// our builder. A tagged value then stays a tagged value (`k: !t x`, read
/// back as the same `Value`) instead of becoming the one-entry map
/// `{'!t': x}` (so `#[serde(flatten)]` round-trips).
struct Unchecked<'a>(&'a Value);

impl ser::Serialize for Unchecked<'_> {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Never the document serializer here: a child is never its root.
        if to_value_serializer::<S>() && armed() {
            return hand_over(s, &Payload::Value(self.0), || {
                super::walk::clone_value(self.0)
            });
        }
        plain(self.0, s)
    }
}

/// The plain serde data of `v` (depth already checked); children go
/// through [`Unchecked`].
fn plain<S: ser::Serializer>(v: &Value, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Value::Null => s.serialize_unit(),
        Value::Bool(b) => s.serialize_bool(*b),
        // Plain data: a child `Number` never takes the hand-over (nor
        // spends the mark) itself; its `Value` already had the chance.
        Value::Number(n) => serialize_number(n, s),
        Value::String(v) => s.serialize_str(v),
        Value::Sequence(seq) => {
            let mut out = s.serialize_seq(Some(seq.len()))?;
            for item in seq.iter() {
                out.serialize_element(&Unchecked(item))?;
            }
            out.end()
        }
        Value::Mapping(m) => {
            let mut out = s.serialize_map(Some(m.len()))?;
            for (k, v) in m {
                out.serialize_entry(&Unchecked(k), &Unchecked(v))?;
            }
            out.end()
        }
        // `{tag: value}`, as `serde_yaml` does for data models without
        // tags.
        Value::Tagged(t) => {
            let mut out = s.serialize_map(Some(1))?;
            out.serialize_entry(&TagText(&t.tag), &Unchecked(&t.value))?;
            out.end()
        }
    }
}

/// What a `Value`, `Sequence`, `Mapping`, or `Number` hands to a serializer inside
/// the private newtype. Serialized only when that serializer did **not**
/// take the captured clone (a third-party serializer reached through a
/// stale [`OURS`] flag): it then serializes the plain data.
enum Payload<'a> {
    Value(&'a Value),
    Sequence(&'a Sequence),
    Mapping(&'a Mapping),
    Number(&'a Number),
}

impl ser::Serialize for Payload<'_> {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // The clone was not taken: never leave it for someone else.
        clear_slot();
        match self {
            Payload::Value(v) => checked(v, s),
            Payload::Sequence(q) => serialize_sequence(q, s),
            Payload::Mapping(m) => serialize_mapping(m, s),
            Payload::Number(n) => serialize_number(n, s),
        }
    }
}

fn clear_slot() {
    if let Ok(Some(left)) = SER_SLOT.try_with(Cell::take) {
        super::walk::drop_value(left);
    }
}

/// Empties [`SER_SLOT`] however a hand-over ends (error, panic, or a
/// serializer that never took it).
struct SlotClear;

impl Drop for SlotClear {
    fn drop(&mut self) {
        clear_slot();
    }
}

/// Marks the next `Serialize` call on this thread as going to oneyaml's
/// own [`ToValue`](crate::ser::build) builder (only then
/// does a `Value` use the private hand-over). `ToValue` calls this right
/// before each nested `serialize`, and [`disarm`] on entry to each of its
/// methods, so the mark is spent by the first thing that reads it.
///
/// The returned guard puts back the mark this call found when it is
/// dropped: a user `Serialize` impl that runs
/// oneyaml itself (`oneyaml::to_string(&1)`) before delegating to a
/// `Value` must not spend the mark its own caller set, or that `Value`
/// would take the plain-data path (`{'!t': x}` instead of `!t x`).
#[must_use = "the mark is restored when the guard drops"]
pub(crate) fn arm() -> Armed {
    let prev = OURS.try_with(|o| o.replace(true)).unwrap_or(false);
    Armed { prev }
}

/// Restores the mark [`arm`] found (on every exit: return, error, or
/// unwinding).
pub(crate) struct Armed {
    prev: bool,
}

impl Drop for Armed {
    fn drop(&mut self) {
        let _ = OURS.try_with(|o| o.set(self.prev));
    }
}

/// Clears the mark set by [`arm`].
pub(crate) fn disarm() {
    let _ = OURS.try_with(|o| o.set(false));
}

/// Takes the mark set by [`arm`].
fn armed() -> bool {
    OURS.try_with(|o| o.replace(false)).unwrap_or(false)
}

/// Whether `S` is oneyaml's `ToValue` builder or its streaming serializer
/// (`type_name` omits lifetimes). Checked together with the
/// mark, so a mark left set while the user's own `Serialize` code calls a
/// third-party serializer is neither used nor spent there.
fn to_value_serializer<S>() -> bool {
    let name = std::any::type_name::<S>();
    name == std::any::type_name::<crate::ser::build::ToValue>()
        || name == std::any::type_name::<crate::ser::stream::Streamer<'static>>()
}

/// Whether `S` is oneyaml's document [`Serializer`](crate::Serializer)
/// (`&mut Serializer<W>`), which writes through the streaming serializer
/// too. A root `Value` serialized into it is never armed (nothing of ours runs
/// before the user's call), so it is recognised by its type name. A false
/// negative would only take the recursive path; a false positive needs a type
/// with the same path, i.e. another copy of oneyaml, whose serializer then
/// serializes the plain data through [`Payload`].
fn document_serializer<S>() -> bool {
    let name = std::any::type_name::<S>();
    let ours = std::any::type_name::<&mut crate::ser::Serializer<()>>();
    ours.strip_suffix("<()>")
        .and_then(|prefix| name.strip_prefix(prefix))
        .is_some_and(|rest| rest.starts_with('<'))
}

/// For `ToValue::serialize_newtype_struct` with [`VALUE_MAGIC`]: the value
/// a `Value`/`Sequence`/`Mapping`/`Number` handed over, or `None` when the newtype
/// was not ours (a user type with a spoofed name then serializes normally,
/// with no flag set: one interpretation).
pub(crate) fn take_handed_over() -> Option<Value> {
    SER_SLOT.try_with(Cell::take).ok().flatten()
}

/// Third-party serializers see plain serde data. Only oneyaml's
/// own builder (armed, or the document `Serializer` at the root) gets the
/// private newtype, with a clone handed over for the iterative emitter.
fn hand_over<S: ser::Serializer>(
    s: S,
    payload: &Payload<'_>,
    clone: impl FnOnce() -> Value,
) -> Result<S::Ok, S::Error> {
    // `try_with`, never `with`: in a thread-local destructor the
    // slot (it holds a `Value`, so it has a destructor) may already be
    // destroyed. `S` is our own builder or
    // document `Serializer` here, so the value is refused with `Y3017`
    // instead of being written as lossy plain data (`{'!t': ...}`). `OURS`
    // has no destructor and stays usable, so every nested `Value` reaches
    // this point too and fails the same way.
    let _clear = SlotClear;
    match SER_SLOT.try_with(|slot| slot.replace(Some(clone()))) {
        Ok(Some(left)) => super::walk::drop_value(left),
        Ok(None) => {}
        Err(_) => return Err(ser::Error::custom(TlsGone)),
    }
    s.serialize_newtype_struct(VALUE_MAGIC, payload)
}

/// The private hand-over only for oneyaml's own builder,
/// flagged by it ([`arm`]), or for the document `Serializer` at the root.
fn ours<S>() -> bool {
    (to_value_serializer::<S>() && armed()) || document_serializer::<S>()
}

impl ser::Serialize for Value {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if ours::<S>() {
            return hand_over(s, &Payload::Value(self), || super::walk::clone_value(self));
        }
        checked(self, s)
    }
}

impl ser::Serialize for Mapping {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if ours::<S>() {
            return hand_over(s, &Payload::Mapping(self), || Value::Mapping(self.clone()));
        }
        serialize_mapping(self, s)
    }
}

impl ser::Serialize for Sequence {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if ours::<S>() {
            return hand_over(s, &Payload::Sequence(self), || {
                Value::Sequence(self.clone())
            });
        }
        serialize_sequence(self, s)
    }
}

fn serialize_mapping<S: ser::Serializer>(m: &Mapping, s: S) -> Result<S::Ok, S::Error> {
    // Depth of the entries, plus this level.
    let limit = serialize_limit().saturating_sub(1);
    let deep = m.iter().any(|(k, v)| {
        super::walk::serde_depth(k, limit) > limit || super::walk::serde_depth(v, limit) > limit
    });
    if deep {
        return Err(too_deep());
    }
    let mut out = s.serialize_map(Some(m.len()))?;
    for (k, v) in m {
        out.serialize_entry(&Unchecked(k), &Unchecked(v))?;
    }
    out.end()
}

fn serialize_sequence<S: ser::Serializer>(q: &Sequence, s: S) -> Result<S::Ok, S::Error> {
    let limit = serialize_limit().saturating_sub(1);
    if q.iter().any(|v| super::walk::serde_depth(v, limit) > limit) {
        return Err(too_deep());
    }
    let mut out = s.serialize_seq(Some(q.len()))?;
    for item in q.iter() {
        out.serialize_element(&Unchecked(item))?;
    }
    out.end()
}

impl ser::Serialize for Number {
    /// Into oneyaml's own serializers (`to_string`, [`Serializer`](crate::Serializer),
    /// and a `Number` field of a type serialized by them): handed over
    /// whole, exactly as `Value::Number` would be, so an integer of any
    /// width is written as a number and reads back as the same `Number`
    /// (round trip).
    ///
    /// Into any other serializer (plain serde data): integers
    /// as the narrowest of `u64`/`i64`/`u128`/`i128` that fits, floats as
    /// `f64`, and integers wider than 128 bits as their text
    /// (`serialize_str`), since serde's data model has no wider integer.
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if ours::<S>() {
            return hand_over(s, &Payload::Number(self), || Value::Number(self.clone()));
        }
        serialize_number(self, s)
    }
}

/// The plain serde data of a `Number` (see its `Serialize` impl).
fn serialize_number<S: ser::Serializer>(n: &Number, s: S) -> Result<S::Ok, S::Error> {
    if let Some(f) = n.as_float() {
        s.serialize_f64(f)
    } else if let Some(u) = n.as_u64() {
        s.serialize_u64(u)
    } else if let Some(i) = n.as_i64() {
        s.serialize_i64(i)
    } else if let Some(u) = n.as_u128() {
        s.serialize_u128(u)
    } else if let Some(i) = n.as_i128() {
        s.serialize_i128(i)
    } else {
        s.serialize_str(&n.to_string())
    }
}

/// A tag as a string key (written from its pieces, never joined
/// by us; a third-party serializer receives it as a string).
struct TagText<'a>(&'a super::Tag);

impl ser::Serialize for TagText<'_> {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    /// A visitor that panics where `Value`'s would take the slot.
    struct Panics;

    impl<'de> Visitor<'de> for Panics {
        type Value = ();
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("nothing")
        }
        #[allow(clippy::panic)]
        fn visit_newtype_struct<D: de::Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
            panic!("visitor panicked");
        }
    }

    /// The fallback of a hand-over that was not taken: plain serde data,
    /// and the slot emptied (`Payload::Number` included).
    #[test]
    fn payload_not_taken_is_plain_data_and_clears_the_slot() {
        let wide: Number = crate::from_str("-123456789012345678901234567890123456789012").unwrap();
        let two = Number::from(2);
        let q: Sequence = vec![Value::from(1)].into();
        let mut m = Mapping::new();
        m.insert(Value::from("a"), Value::from(true));
        let cases = [
            (Payload::Number(&wide), format!("\"{wide}\"")),
            (Payload::Number(&two), "2".to_owned()),
            (Payload::Sequence(&q), "[1]".to_owned()),
            (Payload::Mapping(&m), "{\"a\":true}".to_owned()),
            (Payload::Value(&Value::Null), "null".to_owned()),
        ];
        for (payload, want) in cases {
            SER_SLOT.with(|slot| slot.set(Some(Value::from("left"))));
            assert_eq!(serde_json::to_string(&payload).unwrap(), want);
            assert!(SER_SLOT.with(Cell::take).is_none());
        }
    }

    #[test]
    fn hand_off_never_leaves_a_value_behind() {
        let unwound = std::panic::catch_unwind(|| hand_off(Value::from(1), Panics));
        assert!(unwound.is_err());
        assert!(SLOT.with(Cell::take).is_none());
        // A visitor that takes it, and one that does not.
        assert!(hand_off(Value::from(1), ValueVisitor).is_ok());
        assert!(hand_off(Value::from(2), serde::de::IgnoredAny).is_ok());
        assert!(SLOT.with(Cell::take).is_none());
    }
}
