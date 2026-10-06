//! The mapping keys the edit API's lookups and inserts take:
//! one sealed trait, so `get("a")`, `get(1)`, `get(true)` and
//! `get(&value)` read the same way.

use std::borrow::Cow;

use crate::value::Value;

/// A mapping key for [`Document::get`](crate::Document::get),
/// [`NodeRef::get`](crate::NodeRef::get),
/// [`NodeMut::get_mut`](crate::NodeMut::get_mut),
/// [`NodeMut::insert`](crate::NodeMut::insert) and the other key lookups
/// and inserts of the edit API.
///
/// Implemented for `str`, `String`, the integer types, `bool`, and
/// [`Value`] (and references to each). Sealed: the set of key types is
/// part of the API's meaning.
///
/// What a key matches is what [`Value`] equality says: a string key
/// matches the keys that resolve to that string (`get("1")` does not match
/// the integer key `1`, `get(1)` does); any other key matches the scalar
/// keys whose resolved value equals it (`~`, `null` and an empty key are
/// one key, `0x10` and `16` are one key).
///
/// An error naming a key ([`EditError::at`](crate::EditError::at)) gives
/// a non-string key as its Core text: the integer key `3` and the string
/// key `"3"` both appear as `path!["3"]` ([`Path`](crate::Path) keys are
/// text).
///
/// ```
/// use oneyaml::{Document, Value};
///
/// let doc = Document::parse("1: one\n'1': quoted\ntrue: yes\n~: nothing\n").unwrap();
/// assert_eq!(doc.get(1).unwrap().text(), "one");
/// assert_eq!(doc.get("1").unwrap().text(), "quoted");
/// assert_eq!(doc.get(String::from("1")).unwrap().text(), "quoted");
/// assert_eq!(doc.get(true).unwrap().text(), "yes");
/// assert_eq!(doc.get(&Value::Null).unwrap().text(), "nothing");
/// ```
pub trait AsKey: sealed::Sealed {
    // Sealed: no methods of its own; the key a type gives is internal.
}

pub(crate) use sealed::KeyArg;

mod sealed {
    use super::{Cow, Value};

    /// The key a lookup or insert asks for.
    #[derive(Clone, Debug)]
    pub enum KeyArg<'a> {
        /// A string key.
        Str(&'a str),
        /// A key equal by meaning to a value.
        Value(Cow<'a, Value>),
    }

    pub trait Sealed {
        fn key_arg(&self) -> KeyArg<'_>;
    }
}

impl KeyArg<'_> {
    /// The key as a value (for writing it).
    pub(crate) fn to_value(&self) -> Value {
        match self {
            KeyArg::Str(s) => Value::from(*s),
            KeyArg::Value(v) => v.as_ref().clone(),
        }
    }

    /// The string a string key is (`Value::String` included).
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            KeyArg::Str(s) => Some(s),
            KeyArg::Value(v) => match v.as_ref() {
                Value::String(s) => Some(s),
                _ => None,
            },
        }
    }

    /// The path segment naming the key in an error ([`oneyaml_core::Path`]
    /// keys are text): a string as itself, a scalar as written in Core
    /// form, anything else as `?`. So an error about the integer key `3`
    /// names `path!["3"]`, the same path as for the string key `"3"`
    /// (documented on [`AsKey`]).
    pub(crate) fn label(&self) -> String {
        if let Some(s) = self.as_str() {
            return s.to_owned();
        }
        match self {
            KeyArg::Value(v) => match v.untag_ref() {
                Value::Null => "null".to_owned(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                _ => "?".to_owned(),
            },
            KeyArg::Str(s) => (*s).to_owned(),
        }
    }

    /// Whether the key is a collection (a known limitation: entries with
    /// collection keys are not inserted).
    pub(crate) fn is_collection(&self) -> bool {
        match self {
            KeyArg::Str(_) => false,
            KeyArg::Value(v) => matches!(v.untag_ref(), Value::Sequence(_) | Value::Mapping(_)),
        }
    }
}

impl AsKey for str {}
impl sealed::Sealed for str {
    fn key_arg(&self) -> KeyArg<'_> {
        KeyArg::Str(self)
    }
}

impl AsKey for String {}
impl sealed::Sealed for String {
    fn key_arg(&self) -> KeyArg<'_> {
        KeyArg::Str(self)
    }
}

impl AsKey for Value {}
impl sealed::Sealed for Value {
    fn key_arg(&self) -> KeyArg<'_> {
        KeyArg::Value(Cow::Borrowed(self))
    }
}

impl AsKey for bool {}
impl sealed::Sealed for bool {
    fn key_arg(&self) -> KeyArg<'_> {
        KeyArg::Value(Cow::Owned(Value::Bool(*self)))
    }
}

impl<T: AsKey + ?Sized> AsKey for &T {}
impl<T: sealed::Sealed + ?Sized> sealed::Sealed for &T {
    fn key_arg(&self) -> KeyArg<'_> {
        (**self).key_arg()
    }
}

macro_rules! int_key {
    ($($t:ty),*) => {$(
        impl AsKey for $t {}
        impl sealed::Sealed for $t {
            fn key_arg(&self) -> KeyArg<'_> {
                KeyArg::Value(Cow::Owned(Value::from(*self)))
            }
        }
    )*};
}
int_key!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

/// The key `k` asks for.
pub(crate) fn arg<K: AsKey + ?Sized>(k: &K) -> KeyArg<'_> {
    sealed::Sealed::key_arg(k)
}
