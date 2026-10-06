//! Duplicate-key policy and merge keys (the duplicate-key settings,
//! yaml.org/type/merge), shared by every consumer that builds mappings.
//!
//! **Merge semantics.** A `<<` entry's value is a mapping or a sequence of
//! mappings. Its entries are added to the enclosing mapping unless the
//! mapping has that key explicitly (explicit keys win, wherever they
//! appear); among merged mappings, earlier ones win (left to right). The
//! resulting order is: explicit entries in source order, then merged
//! entries in merge order (the `serde_yaml` order). A second `<<` in one
//! mapping is a duplicate key: an error by default; under
//! `DuplicateKeys::LastWins` the last `<<` replaces the earlier ones.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;

use oneyaml_core::{Diagnostic, DuplicateKeys, Label, Span, code};

use super::merge::MergeAcc;
use crate::error::{Error, warning};
use crate::value::Value;

/// A short, human-readable form of a key for messages and paths, built
/// within its bound (a key may be megabytes, an alias-expanded
/// key a whole collection): at most `MAX_SEGMENT_CHARS` characters of a
/// scalar, 40 of a collection's `Debug`, then `…`.
pub(crate) fn key_text(key: &Value) -> String {
    use crate::error::{MAX_SEGMENT_CHARS, segment, short_chars};
    use crate::value::walk::KeyDebug;
    // An opaque identity (an alias read without expansion) shows as
    // the alias, never as its internal tag.
    if let Value::Tagged(_) = key {
        let text = short_chars(format_args!("{}", KeyDebug(key)), MAX_SEGMENT_CHARS);
        if !text.starts_with("Tagged(") {
            return text;
        }
    }
    match key.untag_ref() {
        Value::String(s) => segment(s),
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => short_chars(format_args!("{n}"), MAX_SEGMENT_CHARS),
        // Iterative `Debug`, shortened.
        other => short_chars(format_args!("{}", KeyDebug(other)), 40),
    }
}

/// Applies the duplicate-key policy to a key equal to an earlier one: the
/// warning to record under `LastWins` (the caller keeps the first position
/// and the last value), or the error.
pub(crate) fn on_duplicate(
    policy: DuplicateKeys,
    key: &str,
    first: Span,
    dup: Span,
) -> Result<Diagnostic, Error> {
    let first_label = Label::new(first, "first defined here");
    match policy {
        DuplicateKeys::LastWins => Ok(warning(
            code::DUPLICATE_KEY_LAST_WINS,
            format!("duplicate key `{key}`: the last value wins"),
            dup,
        )
        .with_secondary(first_label)),
        DuplicateKeys::Error => Err(Error::new(
            code::DUPLICATE_KEY,
            format!("duplicate key `{key}`"),
            Some(dup),
        )
        .with_secondary(first_label)
        .with_help(
            "remove one of the entries, or use DuplicateKeys::LastWins \
             (Compat::serde_yaml()) to keep the last value",
        )),
        // `DuplicateKeys` is `#[non_exhaustive]` in `-core`, so a wildcard is
        // required. A policy added there without handling here must never be
        // read as another policy: it fails closed with Y9000 never a panic), as
        // an unknown `Schema` does in the resolver. (`-core`'s
        // `duplicate_keys_variants_are_all_handled` test trips first.)
        _ => {
            debug_assert!(false, "no duplicate-key handling for {policy:?}");
            Err(Error::internal("unknown duplicate-key policy", Some(dup)))
        }
    }
}

/// Records a duplicate key under `policy` (see [`on_duplicate`]), unless
/// it is a tolerated duplicate at a replayed position
/// ([`Ev::replayed`](super::Ev)): one interpretation, its warning was
/// recorded when that source position was read, and `Document` reports
/// each source duplicate once, so an alias replay does not repeat it.
/// (An error is never skipped: under `DuplicateKeys::Error` the first
/// read already failed.)
pub(crate) fn duplicate_at(
    warnings: &mut crate::diag_cap::Capped,
    policy: DuplicateKeys,
    replayed: bool,
    key: impl FnOnce() -> String,
    first: Span,
    dup: Span,
) -> Result<(), Error> {
    if replayed && policy == DuplicateKeys::LastWins {
        return Ok(());
    }
    warnings.duplicate(policy, key, first, dup)
}

/// The error for a `<<` whose value is not a mapping or a sequence of
/// mappings (the one message for every consumer).
pub(crate) fn invalid_merge(span: Span) -> Error {
    Error::new(
        code::INVALID_MERGE_VALUE,
        "the value of a merge key `<<` must be a mapping or a sequence of mappings",
        Some(span),
    )
    .with_help("turn Settings::merge_keys off to read `<<` as an ordinary key")
}

/// The merged entries of one mapping, pending until the mapping ends: the
/// union of its merge sources, earlier sources first (see
/// [`MergeAcc`]; the builder and the serde planner share it).
pub(crate) struct Merge<K, V> {
    /// Created on the first merge source (most mappings have none); boxed
    /// so mapping states stay small.
    acc: Option<Box<MergeAcc<K, V>>>,
    /// Span of the first `<<` key.
    first: Option<Span>,
}

impl<K, V> Default for Merge<K, V> {
    fn default() -> Self {
        Self {
            acc: None,
            first: None,
        }
    }
}

impl<K: Hash + Eq, V> Merge<K, V> {
    /// A `<<` key at `span` got its value: a repeated `<<` is a duplicate
    /// key (`warnings` receives the `LastWins` warning, unless `span` is a
    /// replayed position, see [`duplicate_at`]; the earlier sources are
    /// dropped). Checked before the value's shape.
    pub(crate) fn key(
        &mut self,
        span: Span,
        replayed: bool,
        policy: DuplicateKeys,
        warnings: &mut crate::diag_cap::Capped,
    ) -> Result<(), Error> {
        if let Some(first) = self.first {
            duplicate_at(warnings, policy, replayed, || "<<".to_owned(), first, span)?;
            self.acc = None;
        } else {
            self.first = Some(span);
        }
        Ok(())
    }

    /// Adds one merge source (a mapping's entries), after the earlier ones.
    pub(crate) fn absorb(&mut self, source: Box<MergeAcc<K, V>>) {
        match self.acc.as_mut() {
            None => self.acc = Some(source),
            Some(acc) => acc.absorb_later(*source),
        }
    }

    /// True when no `<<` was seen.
    pub(crate) fn is_empty(&self) -> bool {
        self.first.is_none()
    }

    /// The merged entries, earlier sources first (none without sources).
    pub(crate) fn into_merged(self) -> Option<Box<MergeAcc<K, V>>> {
        self.acc
    }

    /// The mapping's entries: `explicit`, then the merged entries whose
    /// keys it does not have, in merge order.
    pub(crate) fn finish(self, explicit: MergeAcc<K, V>) -> MergeAcc<K, V> {
        match self.acc {
            None => explicit,
            Some(acc) => explicit.append_later(*acc),
        }
    }
}

/// A key of a mapping being streamed (serde): string keys borrow from the
/// source when their value does.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum KeyRepr<'a> {
    Str(Cow<'a, str>),
    /// Any non-string key (never `Value::String`).
    Other(Value),
}

impl KeyRepr<'_> {
    pub(crate) fn of_value(v: Value) -> Self {
        match v {
            Value::String(s) => KeyRepr::Str(Cow::Owned(s)),
            other => KeyRepr::Other(other),
        }
    }

    /// The key as a `Value` (inverse of [`KeyRepr::of_value`]).
    pub(crate) fn into_value(self) -> Value {
        match self {
            KeyRepr::Str(s) => Value::String(s.into_owned()),
            KeyRepr::Other(v) => v,
        }
    }
}

/// How many keys [`KeyMap`] compares linearly before it indexes them.
const SMALL_KEYS: usize = 8;

/// A key [`KeyMap`] may compare linearly.
pub(crate) trait SmallKey {
    /// Whether comparing this key with `==` is cheap and allocation-free
    /// (a scalar). A collection or tagged key is not: its `==` walks it
    /// with an explicit stack, so [`KeyMap`] hashes it once instead.
    /// Such a key never equals a cheap one (`Value`'s equality
    /// compares variants first), so where a key is held never changes what
    /// is a duplicate.
    fn is_small(&self) -> bool;
}

impl SmallKey for KeyRepr<'_> {
    fn is_small(&self) -> bool {
        match self {
            KeyRepr::Str(_) => true,
            KeyRepr::Other(v) => {
                !matches!(v, Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_))
            }
        }
    }
}

/// The keys of one mapping, for duplicate detection: up to
/// [`SMALL_KEYS`] scalar keys in a `Vec` compared with `==`, then a
/// `HashMap` with the default `RandomState` (content-keyed maps keep
/// it, so hash flooding costs O(1) per key; the linear part is bounded by
/// a constant). Most mappings have a few keys, and hashing
/// each key with SipHash cost more than comparing it with the others.
/// Collection and tagged keys always go to the `HashMap` (see
/// [`SmallKey`]). The two parts use the same equality (`KeyRepr`'s `Eq`,
/// which its `Hash` agrees with), so which part holds a key never changes
/// what is a duplicate.
pub(crate) struct KeyMap<K, V> {
    /// The small keys while there are at most [`SMALL_KEYS`]; empty once
    /// `spilled`.
    small: Vec<(K, V)>,
    /// Every key that is not small, and, once `spilled`, every key.
    big: HashMap<K, V>,
    /// The small keys outgrew `small` and moved to `big`.
    spilled: bool,
}

impl<K, V> Default for KeyMap<K, V> {
    fn default() -> Self {
        Self {
            small: Vec::new(),
            big: HashMap::new(),
            spilled: false,
        }
    }
}

impl<K: Eq + Hash + SmallKey, V> KeyMap<K, V> {
    /// The value recorded for `key`.
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        if self.spilled || !key.is_small() {
            self.big.get(key)
        } else {
            self.small.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        }
    }

    /// Whether `key` is recorded.
    pub(crate) fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Records `key`, which the caller found absent ([`KeyMap::get`]).
    pub(crate) fn insert_new(&mut self, key: K, value: V) {
        if self.spilled || !key.is_small() {
            self.big.insert(key, value);
        } else if self.small.len() < SMALL_KEYS {
            // Grows as the `HashMap` it replaces did (4, then 8 entries): never
            // more allocations or bytes than before (the allocation gates).
            self.small.push((key, value));
        } else {
            // The capacity the `HashMap` would have had by now.
            self.big.reserve(SMALL_KEYS.saturating_add(1));
            self.big.extend(self.small.drain(..));
            self.big.insert(key, value);
            self.spilled = true;
        }
    }

    /// Forgets every key, keeping the allocations (pooled states).
    pub(crate) fn clear(&mut self) {
        self.small.clear();
        self.big.clear();
        self.spilled = false;
    }

    /// Moves every key out, in no particular order.
    pub(crate) fn drain(&mut self) -> impl Iterator<Item = (K, V)> + '_ {
        self.spilled = false;
        self.small.drain(..).chain(self.big.drain())
    }
}

/// The keys of one mapping being streamed, with the span of each first
/// definition. Holds keys only, never values.
#[derive(Default)]
pub(crate) struct KeySet<'a> {
    keys: KeyMap<KeyRepr<'a>, Span>,
}

impl<'a> KeySet<'a> {
    /// Records `key`; returns the first definition's span if it is a
    /// duplicate.
    pub(crate) fn insert(&mut self, key: KeyRepr<'a>, span: Span) -> Option<Span> {
        match self.keys.get(&key) {
            Some(first) => Some(*first),
            None => {
                self.keys.insert_new(key, span);
                None
            }
        }
    }

    /// The first definition's span of `key`, if it was recorded.
    pub(crate) fn first(&self, key: &KeyRepr<'a>) -> Option<Span> {
        self.keys.get(key).copied()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    type M = Merge<Value, Value>;

    fn map(pairs: &[(&str, i32)]) -> MergeAcc<Value, Value> {
        MergeAcc::from_entries(
            pairs
                .iter()
                .map(|(k, v)| (Value::from(*k), Value::from(*v))),
        )
    }

    fn done(m: M, explicit: &[(&str, i32)]) -> Vec<(Value, Value)> {
        m.finish(map(explicit)).into_entries().collect()
    }

    #[test]
    fn merge_left_to_right_explicit_wins() {
        let mut m = M::default();
        let mut w = crate::diag_cap::Capped::with_keep(usize::MAX);
        m.key(Span::empty(0), false, DuplicateKeys::Error, &mut w)
            .unwrap();
        m.absorb(Box::new(map(&[("a", 1), ("b", 1)])));
        m.absorb(Box::new(map(&[("b", 2), ("c", 2)])));
        let out = done(m, &[("c", 3)]);
        assert_eq!(
            out,
            vec![
                (Value::from("c"), Value::from(3)),
                (Value::from("a"), Value::from(1)),
                (Value::from("b"), Value::from(1))
            ]
        );
        assert_eq!(w.total(), 0);
    }

    #[test]
    fn merge_errors_and_repeats() {
        assert_eq!(
            invalid_merge(Span::new(1, 2)).code(),
            code::INVALID_MERGE_VALUE
        );
        let mut w = crate::diag_cap::Capped::with_keep(usize::MAX);
        let mut m = M::default();
        assert!(m.is_empty());
        m.key(Span::empty(0), false, DuplicateKeys::LastWins, &mut w)
            .unwrap();
        m.absorb(Box::new(map(&[("a", 1)])));
        m.key(Span::empty(5), false, DuplicateKeys::LastWins, &mut w)
            .unwrap();
        m.absorb(Box::new(map(&[("b", 2)])));
        let (kept, _) = w.take();
        assert_eq!(kept[0].code, code::DUPLICATE_KEY_LAST_WINS);
        assert_eq!(kept[0].secondary[0].message, "first defined here");
        assert!(!m.is_empty());
        assert_eq!(done(m, &[]), vec![(Value::from("b"), Value::from(2))]);
        let mut m = M::default();
        m.key(Span::empty(0), false, DuplicateKeys::Error, &mut w)
            .unwrap();
        let e = m
            .key(Span::empty(3), false, DuplicateKeys::Error, &mut w)
            .unwrap_err();
        assert_eq!(e.code(), code::DUPLICATE_KEY);
    }

    #[test]
    fn key_set_and_text() {
        let mut s = KeySet::default();
        assert_eq!(s.insert(KeyRepr::Str("a".into()), Span::new(0, 1)), None);
        assert_eq!(
            s.insert(KeyRepr::of_value(Value::from("a")), Span::new(5, 6)),
            Some(Span::new(0, 1))
        );
        assert_eq!(
            s.insert(KeyRepr::of_value(Value::from(1)), Span::new(7, 8)),
            None
        );
        assert_eq!(key_text(&Value::Null), "null");
        assert_eq!(key_text(&Value::from(true)), "true");
        assert_eq!(key_text(&Value::from(1.5)), "1.5");
        let long = Value::from(vec![Value::from("x".repeat(80))]);
        assert!(key_text(&long).ends_with('…'));
    }

    #[test]
    fn collection_and_tagged_keys_are_hashed_from_the_start() {
        // Never compared linearly; scalar keys stay linear.
        let seq = |i: i64| KeyRepr::of_value(Value::from(vec![Value::from(i)]));
        let tagged = |t: &str| {
            KeyRepr::of_value(Value::Tagged(Box::new(crate::value::TaggedValue {
                tag: crate::value::Tag::new("!t"),
                value: Value::from(t),
            })))
        };
        assert!(!seq(0).is_small() && !tagged("x").is_small());
        assert!(KeyRepr::Str("x".into()).is_small());
        assert!(KeyRepr::of_value(Value::Null).is_small());
        let mut m: KeyMap<KeyRepr<'_>, usize> = KeyMap::default();
        for i in 0..20 {
            m.insert_new(seq(i), 0);
        }
        m.insert_new(tagged("x"), 1);
        m.insert_new(KeyRepr::Str("x".into()), 2);
        assert_eq!((m.small.len(), m.big.len(), m.spilled), (1, 21, false));
        assert_eq!(m.get(&seq(19)), Some(&0));
        assert_eq!(m.get(&tagged("x")), Some(&1));
        assert_eq!(m.get(&KeyRepr::Str("x".into())), Some(&2));
        assert!(!m.contains_key(&tagged("y")) && !m.contains_key(&seq(20)));
        // Spilling moves the scalar keys only once there are enough.
        for i in 0..SMALL_KEYS {
            m.insert_new(KeyRepr::of_value(Value::from(i as i64)), 3);
        }
        assert!(m.spilled && m.small.is_empty());
        assert_eq!(m.get(&KeyRepr::Str("x".into())), Some(&2));
        assert_eq!(m.get(&seq(3)), Some(&0));
        assert_eq!(m.drain().count(), 22 + SMALL_KEYS);
        assert!(!m.spilled);
        m.insert_new(KeyRepr::Str("y".into()), 4);
        assert_eq!(m.small.len(), 1);
    }

    #[test]
    fn key_map_finds_the_same_duplicates_below_and_past_the_linear_bound() {
        // Which part holds the keys never changes the answer.
        for n in [0usize, 1, SMALL_KEYS - 1, SMALL_KEYS, SMALL_KEYS + 1, 40] {
            let mut m: KeyMap<KeyRepr<'_>, usize> = KeyMap::default();
            for i in 0..n {
                let k = KeyRepr::of_value(Value::from(i as i64));
                assert!(!m.contains_key(&k), "{n} {i}");
                m.insert_new(k, i);
            }
            for i in 0..n {
                let k = KeyRepr::of_value(Value::from(i as i64));
                assert_eq!(m.get(&k), Some(&i), "{n} {i}");
                // A string with the same text is another key.
                assert!(!m.contains_key(&KeyRepr::Str(i.to_string().into())));
            }
            // Meaning-only equality: order-independent mappings.
            let a = Value::Mapping(crate::value::Mapping::from_iter([
                (Value::from("a"), Value::from(1)),
                (Value::from("b"), Value::from(2)),
            ]));
            let b = Value::Mapping(crate::value::Mapping::from_iter([
                (Value::from("b"), Value::from(2)),
                (Value::from("a"), Value::from(1)),
            ]));
            m.insert_new(KeyRepr::of_value(a), 999);
            assert_eq!(m.get(&KeyRepr::of_value(b)), Some(&999), "{n}");
            let mut drained: Vec<usize> = m.drain().map(|(_, v)| v).collect();
            drained.sort_unstable();
            let mut want: Vec<usize> = (0..n).collect();
            want.push(999);
            assert_eq!(drained, want);
            assert!(m.get(&KeyRepr::of_value(Value::from(0))).is_none());
            // Cleared and reused (pooled states): starts small again.
            m.insert_new(KeyRepr::Str("x".into()), 1);
            m.clear();
            assert!(!m.contains_key(&KeyRepr::Str("x".into())));
        }
    }
}
