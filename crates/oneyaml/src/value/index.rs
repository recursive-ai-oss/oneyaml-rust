//! Non-panicking read-only indexing into [`Value`].

use std::ops;

use super::{KeyRef, Value};

/// Something that can index a [`Value`]: a `usize` (sequence position), a
/// `str`/`String` (mapping key), or a `Value` (any mapping key).
///
/// Sealed: implemented only by this crate.
pub trait ValueIndex: private::Sealed {
    #[doc(hidden)]
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value>;
    #[doc(hidden)]
    fn index_into_mut<'v>(&self, v: &'v mut Value) -> Option<&'v mut Value>;
}

mod private {
    pub trait Sealed {}
    impl Sealed for usize {}
    impl Sealed for str {}
    impl Sealed for String {}
    impl Sealed for super::Value {}
    impl<T: Sealed + ?Sized> Sealed for &T {}
}

impl ValueIndex for usize {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        match v.untag_ref() {
            Value::Sequence(s) => s.items.get(*self),
            _ => None,
        }
    }
    fn index_into_mut<'v>(&self, v: &'v mut Value) -> Option<&'v mut Value> {
        match v.untag_mut() {
            Value::Sequence(s) => s.items.get_mut(*self),
            _ => None,
        }
    }
}

fn by_key<'v>(v: &'v Value, key: KeyRef<'_>) -> Option<&'v Value> {
    match v.untag_ref() {
        Value::Mapping(m) => m.find(key).and_then(|i| m.entries.get(i)).map(|e| &e.value),
        _ => None,
    }
}

fn by_key_mut<'v>(v: &'v mut Value, key: KeyRef<'_>) -> Option<&'v mut Value> {
    match v.untag_mut() {
        Value::Mapping(m) => m.get_mut_by(key),
        _ => None,
    }
}

impl ValueIndex for str {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        by_key(v, KeyRef::Str(self))
    }
    fn index_into_mut<'v>(&self, v: &'v mut Value) -> Option<&'v mut Value> {
        by_key_mut(v, KeyRef::Str(self))
    }
}

impl ValueIndex for String {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        self.as_str().index_into(v)
    }
    fn index_into_mut<'v>(&self, v: &'v mut Value) -> Option<&'v mut Value> {
        self.as_str().index_into_mut(v)
    }
}

impl ValueIndex for Value {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        by_key(v, KeyRef::Value(self))
    }
    fn index_into_mut<'v>(&self, v: &'v mut Value) -> Option<&'v mut Value> {
        by_key_mut(v, KeyRef::Value(self))
    }
}

impl<T: ValueIndex + ?Sized> ValueIndex for &T {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        (**self).index_into(v)
    }
    fn index_into_mut<'v>(&self, v: &'v mut Value) -> Option<&'v mut Value> {
        (**self).index_into_mut(v)
    }
}

/// The placeholder returned for a missing index.
static MISSING: Value = Value::Null;

/// Read-only indexing that never panics: a missing index, a wrong
/// index type, or a non-collection yields `&Value::Null`. Use
/// [`Value::get`] to tell "missing" from "present and null". There is
/// deliberately no `IndexMut` (no implicit auto-creation).
impl<I: ValueIndex> ops::Index<I> for Value {
    type Output = Value;
    fn index(&self, index: I) -> &Value {
        index.index_into(self).unwrap_or(&MISSING)
    }
}
