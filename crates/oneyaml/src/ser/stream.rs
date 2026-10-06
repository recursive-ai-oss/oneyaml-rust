//! Streaming typed serialization.
//!
//! [`Streamer`] is the `serde::Serializer` behind `to_string`, `to_writer`,
//! and the document [`Serializer`](crate::Serializer): it writes each node
//! as serde's calls arrive, through the one emitter's primitives
//! ([`crate::emit::Emitter`]: slots, block openings, string nodes, keys,
//! tags, document starts), so the output is byte-for-byte what building
//! the value's [`Value`] ([`super::build`]) and emitting it writes, with
//! one exception: under [`Rules::tagged_scalar_variants`]
//! (`Layout::SerdeYaml` read back with
//! `Settings::resolve_tagged_variant_content`) a newtype variant
//! around a null, bool, or number is written `!Name value`, a form a
//! `Value` cannot hold (its tagged scalars are strings), so the build keeps
//! the map form `Name: value` there. No
//! intermediate `Value` of the document is built (that build was
//! 43% of a typed write, and the document existed twice in memory).
//!
//! # Look-ahead, without buffering
//!
//! * **Empty collections** (`[]`/`{}`): a collection's block opening is
//!   deferred to its first entry ([`Frame`]); `end` with no entries writes
//!   the flow form.
//! * **Enum variants** (the `!Name content` / `Name: content` choice of
//!   [`super::build`]): a newtype variant's form depends on its content's
//!   kind, known at the content's first serializer call. Pending variant
//!   names wait in [`DocState::variants`] and are written by that call
//!   ([`open`]), innermost decision first.
//! * **Keys** (implicit or `? `, collection keys, `Layout::SerdeYaml`'s
//!   `[]:` keys, duplicate detection): a map key is built as a `Value`
//!   (bounded by `max_typed_depth`), written with the emitter's key rule,
//!   and kept for duplicate detection; struct field names are
//!   `&'static str` and are kept without allocating.
//! * **Block scalars and quoting** need only the string and its slot.
//! * **Documents** start without look-ahead ([`crate::emit::document_start`]).
//!
//! # Errors: a failed document writes nothing
//!
//! Output goes to the document's buffer ([`DocState::out`]), which the
//! entry points hand out or write only when the whole document succeeded.
//! Errors of the value (serde's side: `Y3012`, `Y3014`, `Y3016`, `Y2007`)
//! return at once, as the `Value` build did. Errors of the output
//! (`Y2005` past `Limits::max_depth`, `Y3015`) are recorded and returned
//! when the document ends, `Y2005` first: the `Value` path reported them
//! only after the build succeeded, and checked the depth before writing,
//! so the error code is the same on both paths. The depth of a node (of
//! a handed-over `Value` or map key: one walk that stops at the limit) is
//! checked before it is written, and once anything is too deep nothing
//! more is appended: refusing never costs the deep value's
//! output, which is quadratic for mapping chains. A `Serialize` impl that
//! swallows an error and goes on gets `Y3016` at the end (its partial text
//! is already written; serde leaves a serializer's state unspecified after
//! an error).
//!
//! # Memory bound
//!
//! The output, plus O(depth) frames on serde's own recursion (bounded by
//! `Limits::max_typed_depth`; no recursion of ours), plus the keys of
//! the open mappings (for duplicate detection: up to 8 struct field names
//! inline per struct, then a set; map keys as `Value`s: O(width) per open
//! mapping), plus a handed-over
//! `Value`, which the caller's data already holds and which is cloned once
//! by its `Serialize` impl as before.
//! A handed-over scalar is written straight through; collections, tags
//! and collection keys are walked (and dropped) on stacks reused for the
//! whole document (`crate::emit::Stacks`), so a `Value` inside
//! typed data costs no allocation beyond its own clone.
//!
//! The per-key cost of a wide map: past `LINEAR_KEYS` (8) keys, each key is
//! held with its cached structural digest (`KeyEntry`, a `u64` next to the
//! `Value`), so a collection key is walked once, not on every probe. That
//! makes the heap peak of a write dominated by one wide map 12–14% higher
//! than at 0.4 (still O(width), the bound above);
//! accepted for the linear cost of collection keys (a key set without the
//! digest re-walks a collection key on every hash).

use std::collections::HashSet;

use serde::ser::{self, Serialize};

use oneyaml_core::{Limits, code};

use super::build::{self, ToValue, f32_value, is_scalar};
use crate::emit::scalar::Rules;
use crate::emit::{Emitter, Slot, Stacks, Start, document_start};
use crate::error::Error;
use crate::value::{DepthExceeded, Number, VALUE_MAGIC, Value, arm, disarm, take_handed_over};

/// One document being written: its output and the state its nodes share.
/// Reused across the documents of a [`Serializer`](crate::Serializer)
/// (the buffer keeps its capacity).
pub(crate) struct DocState {
    /// The document's text so far.
    pub(crate) out: String,
    /// Newtype, tuple, and struct variants waiting for their content's
    /// kind, outermost first; the flag is the decided form (`true`: the
    /// `!Name` tag, `false`: a one-entry mapping `Name:`).
    variants: Vec<(&'static str, bool)>,
    /// `Limits::max_depth`.
    max_depth: usize,
    /// `Limits::max_typed_depth`.
    max_typed: usize,
    /// Some node nests deeper than `max_depth` (`Y2005` at the end).
    too_deep: bool,
    /// The first output error (`Y3015`, or `Internal`).
    failed: Option<Error>,
    /// A serde-side error was returned for this document.
    poisoned: bool,
    /// Stacks reused by every `Value` walk of the document.
    stacks: Stacks,
    /// The stack handed-over `Value`s are dropped on.
    drops: Vec<Value>,
}

impl DocState {
    /// State for documents under `limits`.
    pub(crate) fn new(limits: &Limits) -> Self {
        Self {
            out: String::new(),
            variants: Vec::new(),
            max_depth: limits.get_max_depth(),
            max_typed: limits.get_max_typed_depth(),
            too_deep: false,
            failed: None,
            poisoned: false,
            stacks: Stacks::default(),
            drops: Vec::new(),
        }
    }

    /// Starts a document: clears the state and writes the start marker.
    fn begin(&mut self, rules: &Rules, start: Start) -> At {
        self.out.clear();
        self.variants.clear();
        self.too_deep = false;
        self.failed = None;
        self.poisoned = false;
        let slot = document_start(&mut self.out, rules, start);
        At {
            slot,
            depth: 0,
            typed: ToValue::root(self.max_typed),
        }
    }

    /// Writes `value` as one whole document into [`DocState::out`].
    ///
    /// # Errors
    ///
    /// As [`DocState::finish`], or the value's own error; `out` then holds
    /// a partial document the caller discards.
    pub(crate) fn document<T: ?Sized + Serialize>(
        &mut self,
        rules: &Rules,
        start: Start,
        value: &T,
    ) -> Result<(), Error> {
        let at = self.begin(rules, start);
        emit(value, self, rules, at)?;
        self.finish()
    }

    /// The root serializer of a document started with [`DocState::begin`].
    pub(crate) fn root<'b>(&'b mut self, rules: &'b Rules, start: Start) -> Streamer<'b> {
        let at = self.begin(rules, start);
        Streamer {
            st: self,
            rules,
            at,
        }
    }

    /// Ends the document: the recorded output error, or the final line
    /// break.
    ///
    /// # Errors
    ///
    /// `Y3016` when the value's `Serialize` impl went on after an error;
    /// else `Y2005` when a node nests deeper than `Limits::max_depth`;
    /// else the first `Y3015`.
    pub(crate) fn finish(&mut self) -> Result<(), Error> {
        // A swallowed error decides first: what was written after it is
        // not what the value means.
        if self.poisoned {
            return Err(swallowed());
        }
        if self.too_deep {
            return Err(DepthExceeded {
                limit: self.max_depth,
            }
            .into_error());
        }
        if let Some(e) = self.failed.take() {
            return Err(e);
        }
        if !self.variants.is_empty() {
            // Every successful `serialize` writes one node, which
            // takes the pending variants; none can be left over.
            debug_assert!(false, "enum variants left without content");
            return Err(Error::internal("enum variants left without content", None));
        }
        self.out.push('\n');
        Ok(())
    }

    /// A node `depth` collections deep.
    fn deeper(&mut self, depth: usize) {
        if depth > self.max_depth {
            self.too_deep = true;
        }
    }

    /// Whether output is still written: once something is too deep, the
    /// document fails with `Y2005` and nothing more is appended (refusing
    /// costs no output).
    fn live(&self) -> bool {
        !self.too_deep
    }

    /// Records an output error (the first one is reported).
    fn record(&mut self, r: Result<(), Error>) {
        if let Err(e) = r {
            if self.failed.is_none() {
                self.failed = Some(e);
            }
        }
    }

    /// Marks the document failed on the serde side (see the module docs).
    fn guard<T>(&mut self, r: Result<T, Error>) -> Result<T, Error> {
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }
}

/// `Y3016`: the value's `Serialize` impl swallowed an error and went on.
fn swallowed() -> Error {
    Error::new(
        code::INVALID_SERIALIZE_CALLS,
        "the value's Serialize impl went on after an error; the document is refused",
        None,
    )
}

/// Where a node goes.
#[derive(Clone, Copy, Debug)]
struct At {
    slot: Slot,
    /// Collections enclosing the node (`Value::depth` counting).
    depth: usize,
    /// serde nesting so far (`Limits::max_typed_depth`, as the build).
    typed: ToValue,
}

/// A node's kind, as far as an enum variant's form depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Str,
    Seq,
    Map,
    /// A null, bool, or number.
    Scalar,
    /// A tagged value (a node takes one tag).
    Other,
}

impl Shape {
    fn of(v: &Value) -> Self {
        match v {
            Value::String(_) => Shape::Str,
            Value::Sequence(_) => Shape::Seq,
            Value::Mapping(_) => Shape::Map,
            Value::Null | Value::Bool(_) | Value::Number(_) => Shape::Scalar,
            _ => Shape::Other,
        }
    }
}

/// What is below a node, for the depth check that precedes writing it.
#[derive(Clone, Copy)]
enum Below<'v> {
    /// A scalar.
    Leaf,
    /// A collection opened by serde (one level).
    Collection,
    /// A handed-over `Value`.
    Value(&'v Value),
}

/// The collection depth of `v` (`Value::depth`), walked only when `v` is
/// a collection (a scalar costs no walk and no allocation), and no
/// further than `cap`.
fn value_depth(stacks: &mut Stacks, v: &Value, cap: usize) -> usize {
    match v.untag_ref() {
        Value::Sequence(_) | Value::Mapping(_) => stacks.depth(v, cap),
        _ => 0,
    }
}

/// Where a node's content goes once its pending variants are written.
#[derive(Clone, Copy, Debug)]
struct Placed {
    slot: Slot,
    /// A tag (a variant's `!Name`) was written for the content.
    tagged: bool,
    depth: usize,
}

/// Checks `Limits::max_depth` for a node of `shape` at `at` with `below` under
/// it, then (only when within it) writes the pending enum variants around it
/// (the form [`super::build`]'s `variant` gives each: `!Name` when its content
/// is a string, sequence, or mapping and the name is not empty, else the
/// mapping `Name: content`; under [`Rules::tagged_scalar_variants`] a null,
/// bool, or number content is tagged too), and returns where the content goes.
/// The depth is known before anything is written.
fn open(st: &mut DocState, rules: &Rules, at: At, shape: Shape, below: Below<'_>) -> Placed {
    // Innermost first: a tagged variant is a tagged value (its parent
    // variant cannot be a tag), a mapping-form one is a mapping.
    let mut wrappers = 0usize;
    // `!Name 2.0` only when it reads back as the number (the
    // `serde_yaml` layout read with `resolve_tagged_variant_content`).
    let mut taggable = match shape {
        Shape::Str | Shape::Seq | Shape::Map => true,
        Shape::Scalar => rules.tagged_scalar_variants(),
        Shape::Other => false,
    };
    for (name, form) in st.variants.iter_mut().rev() {
        *form = taggable && !name.is_empty();
        taggable = !*form;
        if !*form {
            wrappers = wrappers.saturating_add(1);
        }
    }
    let depth = at.depth.saturating_add(wrappers);
    let inner = match below {
        Below::Leaf => 0,
        Below::Collection => 1,
        Below::Value(v) => value_depth(&mut st.stacks, v, st.max_depth.saturating_sub(depth)),
    };
    if wrappers > 0 || inner > 0 {
        st.deeper(depth.saturating_add(inner));
    }
    let mut p = Placed {
        slot: at.slot,
        tagged: false,
        depth,
    };
    if st.variants.is_empty() {
        return p;
    }
    if !st.live() {
        st.variants.clear();
        return p;
    }
    let mut em = Emitter::new(&mut st.out, rules);
    let mut failed = Ok(());
    for (name, tag) in st.variants.drain(..) {
        if tag {
            if let Err(e) = em.local_tag(p.slot, name) {
                failed = Err(e);
            }
            p.tagged = true;
        } else {
            let col = p.slot.map_col;
            em.open_block(p.slot, p.tagged, col);
            p.slot = em.str_key(name, col);
            p.tagged = false;
        }
    }
    st.record(failed);
    p
}

/// Serializes a child `v` at `at` (armed for a `Value` hand-over, like
/// [`build::build`]).
fn emit<T: ?Sized + Serialize>(
    v: &T,
    st: &mut DocState,
    rules: &Rules,
    at: At,
) -> Result<(), Error> {
    let r = {
        let _armed = arm();
        v.serialize(Streamer {
            st: &mut *st,
            rules,
            at,
        })
    };
    st.guard(r)
}

/// Opens a collection of `shape` at `at` (one typed level, as the build's
/// `Compound::new`).
fn frame(st: &mut DocState, rules: &Rules, at: At, shape: Shape) -> Result<Frame, Error> {
    let child = at.typed.nested()?;
    let p = open(st, rules, at, shape, Below::Collection);
    let depth = p.depth.saturating_add(1);
    let (col, empty) = if shape == Shape::Seq {
        (p.slot.seq_col, "[]")
    } else {
        (p.slot.map_col, "{}")
    };
    Ok(Frame {
        slot: p.slot,
        tagged: p.tagged,
        col,
        depth,
        child,
        count: 0,
        empty,
        keys: Keys::default(),
        key: None,
    })
}

/// Writes a handed-over `Value` at `at` with the `Value` walk, after
/// checking its depth (a walk that stops at the limit): a value past
/// `max_depth` is refused without writing any of it.
fn handed(st: &mut DocState, rules: &Rules, at: At, v: &Value) {
    let p = open(st, rules, at, Shape::of(v), Below::Value(v));
    if !st.live() {
        return;
    }
    // An untagged scalar is written straight through the
    // primitives the `Value` walk uses for it (`Emitter::content`); only
    // collections and tagged values take the walk, on the reused stack.
    let r = match v {
        Value::String(text) => {
            Emitter::new(&mut st.out, rules).string(p.slot, p.tagged, text);
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {
            Emitter::new(&mut st.out, rules).plain(p.slot, p.tagged, v)
        }
        _ => {
            let mut tasks = st.stacks.take_tasks();
            let r = Emitter::new(&mut st.out, rules).run_with(v, p.slot, p.tagged, &mut tasks);
            st.stacks.put_tasks(tasks);
            r
        }
    };
    st.record(r);
}

/// An open block collection: its opening waits for the first entry.
pub(crate) struct Frame {
    /// Where the collection itself goes.
    slot: Slot,
    tagged: bool,
    /// Column of its entries.
    col: usize,
    /// Its depth (its entries are this many collections deep).
    depth: usize,
    /// serde nesting of its entries.
    child: ToValue,
    count: usize,
    /// Written for no entries.
    empty: &'static str,
    keys: Keys,
    /// A map key waiting for its value.
    key: Option<Value>,
}

impl Frame {
    /// Starts the next entry's line (nothing is written once the document
    /// is too deep).
    fn entry(&mut self, st: &mut DocState, rules: &Rules) -> bool {
        let first = self.count == 0;
        self.count = self.count.saturating_add(1);
        if !st.live() {
            return false;
        }
        let mut em = Emitter::new(&mut st.out, rules);
        if first {
            em.open_block(self.slot, self.tagged, self.col);
        } else {
            em.newline(self.col);
        }
        true
    }

    fn at(&self, slot: Slot) -> At {
        At {
            slot,
            depth: self.depth,
            typed: self.child,
        }
    }

    /// A sequence element.
    pub(crate) fn element<T: ?Sized + Serialize>(
        &mut self,
        st: &mut DocState,
        rules: &Rules,
        v: &T,
    ) -> Result<(), Error> {
        let index = self.count;
        if self.entry(st, rules) {
            Emitter::new(&mut st.out, rules).push_str("- ");
        }
        let at = self.at(Slot::inline(self.col, rules));
        emit(v, st, rules, at).map_err(|e| e.in_index(index))
    }

    /// A struct field.
    pub(crate) fn field<T: ?Sized + Serialize>(
        &mut self,
        st: &mut DocState,
        rules: &Rules,
        name: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        let slot = if self.entry(st, rules) {
            Emitter::new(&mut st.out, rules).str_key(name, self.col)
        } else {
            Slot::value(self.col, rules)
        };
        emit(v, st, rules, self.at(slot)).map_err(|e| e.in_key(name))?;
        if self.keys.insert_name(name) {
            Ok(())
        } else {
            st.guard(Err(build::duplicate_key(Some(name))))
        }
    }

    /// A map key: built as a `Value`, written with its value.
    pub(crate) fn key<T: ?Sized + Serialize>(
        &mut self,
        st: &mut DocState,
        k: &T,
    ) -> Result<(), Error> {
        if self.key.is_some() {
            return st.guard(Err(build::key_without_value()));
        }
        let k = build::build(k, self.child);
        self.key = Some(st.guard(k)?);
        Ok(())
    }

    /// A map value, for the key before it.
    pub(crate) fn value<T: ?Sized + Serialize>(
        &mut self,
        st: &mut DocState,
        rules: &Rules,
        v: &T,
    ) -> Result<(), Error> {
        let Some(k) = self.key.take() else {
            return st.guard(Err(build::value_without_key()));
        };
        // The key's own depth is checked before it is written; a
        // scalar key costs no walk.
        let d = value_depth(&mut st.stacks, &k, st.max_depth.saturating_sub(self.depth));
        if d > 0 {
            st.deeper(self.depth.saturating_add(d));
        }
        let slot = if self.entry(st, rules) {
            let mut tasks = st.stacks.take_tasks();
            let written = Emitter::new(&mut st.out, rules).key(&k, self.col, &mut tasks);
            st.stacks.put_tasks(tasks);
            written.unwrap_or_else(|e| {
                st.record(Err(e));
                Slot::value(self.col, rules)
            })
        } else {
            Slot::value(self.col, rules)
        };
        if let Err(e) = emit(v, st, rules, self.at(slot)) {
            return Err(match k.as_str() {
                Some(n) => e.in_key(n),
                None => e,
            });
        }
        match self.keys.insert_value(k) {
            None => Ok(()),
            Some(k) => st.guard(Err(build::duplicate_key(k.as_str()))),
        }
    }

    /// Ends the collection.
    pub(crate) fn end(self, st: &mut DocState, rules: &Rules) -> Result<(), Error> {
        if self.key.is_some() {
            return st.guard(Err(build::key_without_value()));
        }
        if self.count == 0 && st.live() {
            Emitter::new(&mut st.out, rules).inline_text(self.slot, self.tagged, self.empty);
        }
        Ok(())
    }
}

/// Struct field names held inline before a set is needed.
const INLINE_NAMES: usize = 8;

/// Map keys compared one by one before a set is needed.
const LINEAR_KEYS: usize = 8;

/// The keys of an open mapping, for duplicate detection (`Y3014`), with
/// `Value` equality (order-independent for collection keys), as the
/// build's `Mapping` did. Sets use `RandomState` (keyed by content).
#[derive(Default)]
struct Keys {
    names: [&'static str; INLINE_NAMES],
    named: usize,
    name_set: Option<HashSet<&'static str>>,
    /// The first [`LINEAR_KEYS`] map keys, compared one by one; the
    /// digests of the collection (and tagged) keys among them are held
    /// inline (a `Vec<KeyEntry>` would allocate 20% more bytes
    /// per small mapping, and scalar keys compare cheaper than they
    /// digest).
    values: Vec<Value>,
    digests: [Option<u64>; LINEAR_KEYS],
    value_set: Option<HashSet<KeyEntry>>,
}

/// A map key with its structural digest (`walk::digest`, the hash
/// `Value` uses), computed once per key (a collection
/// key's digest is a walk; `contains` and `insert` hash the cached `u64`).
struct KeyEntry {
    digest: u64,
    key: Value,
}

impl PartialEq for KeyEntry {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest && self.key == other.key
    }
}

impl Eq for KeyEntry {}

impl std::hash::Hash for KeyEntry {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.digest);
    }
}

impl Keys {
    /// Adds a struct field name; `false` if it was there.
    fn insert_name(&mut self, name: &'static str) -> bool {
        if let Some(set) = &mut self.name_set {
            return set.insert(name);
        }
        let held = self.names.get(..self.named).unwrap_or(&[]);
        if held.contains(&name) {
            return false;
        }
        if let Some(slot) = self.names.get_mut(self.named) {
            *slot = name;
            self.named = self.named.saturating_add(1);
            return true;
        }
        let mut set: HashSet<&'static str> = self.names.iter().copied().collect();
        set.insert(name);
        self.name_set = Some(set);
        true
    }

    /// Adds a map key; hands it back if an equal key was there. A key's
    /// digest (a walk for a collection key) is computed at most once.
    fn insert_value(&mut self, key: Value) -> Option<Value> {
        if let Some(set) = &mut self.value_set {
            let e = KeyEntry {
                digest: crate::value::walk::digest(&key),
                key,
            };
            if set.contains(&e) {
                return Some(e.key);
            }
            set.insert(e);
            return None;
        }
        // Scalars compare directly (no walk, no allocation); a collection
        // or tagged key is compared by digest first, so `==` walks only
        // keys that are almost surely equal.
        let digest = match key {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
            _ => Some(crate::value::walk::digest(&key)),
        };
        let held = self.values.iter().zip(self.digests.iter());
        let seen = held.into_iter().any(|(k, d)| match (digest, d) {
            (None, None) => *k == key,
            (Some(a), Some(b)) => a == *b && *k == key,
            _ => false,
        });
        if seen {
            return Some(key);
        }
        if let Some(slot) = self.digests.get_mut(self.values.len()) {
            *slot = digest;
            self.values.push(key);
            return None;
        }
        let mut set: HashSet<KeyEntry> = std::mem::take(&mut self.values)
            .into_iter()
            .zip(self.digests)
            .map(|(key, digest)| KeyEntry {
                digest: digest.unwrap_or_else(|| crate::value::walk::digest(&key)),
                key,
            })
            .collect();
        set.insert(KeyEntry {
            digest: digest.unwrap_or_else(|| crate::value::walk::digest(&key)),
            key,
        });
        self.value_set = Some(set);
        None
    }
}

/// The streaming `serde::Serializer` (see the module docs). Its type name
/// marks it as oneyaml's own for the `Value` hand-over
/// (`crate::value::serde_impl`).
pub(crate) struct Streamer<'b> {
    st: &'b mut DocState,
    rules: &'b Rules,
    at: At,
}

impl<'b> Streamer<'b> {
    fn nested(&self) -> Result<At, Error> {
        Ok(At {
            typed: self.at.typed.nested()?,
            ..self.at
        })
    }

    fn string(self, s: &str) -> Result<(), Error> {
        let p = open(self.st, self.rules, self.at, Shape::Str, Below::Leaf);
        if self.st.live() {
            Emitter::new(&mut self.st.out, self.rules).string(p.slot, p.tagged, s);
        }
        Ok(())
    }

    /// A null, bool, or number (a variant around one is a mapping, or
    /// under [`Rules::tagged_scalar_variants`] a tagged scalar).
    fn plain(self, v: &Value) -> Result<(), Error> {
        let p = open(self.st, self.rules, self.at, Shape::Scalar, Below::Leaf);
        if self.st.live() {
            let r = Emitter::new(&mut self.st.out, self.rules).plain(p.slot, p.tagged, v);
            self.st.record(r);
        }
        Ok(())
    }

    fn number(self, n: Number) -> Result<(), Error> {
        let p = open(self.st, self.rules, self.at, Shape::Scalar, Below::Leaf);
        if self.st.live() {
            Emitter::new(&mut self.st.out, self.rules).separate(p.slot, p.tagged);
            let r = self.rules.write_number(&mut self.st.out, &n);
            self.st.record(r);
        }
        Ok(())
    }

    fn collection(self, shape: Shape) -> Result<Compound<'b>, Error> {
        let Streamer { st, rules, at } = self;
        let frame = frame(st, rules, at, shape)?;
        Ok(Compound { st, rules, frame })
    }

    fn variant_collection(self, name: &'static str, shape: Shape) -> Result<Compound<'b>, Error> {
        let Streamer { st, rules, at } = self;
        let pending = st.variants.len();
        st.variants.push((name, false));
        match frame(st, rules, at, shape) {
            Ok(frame) => Ok(Compound { st, rules, frame }),
            Err(e) => {
                st.variants.truncate(pending);
                Err(e)
            }
        }
    }

    /// The document root as a [`Frame`] (the document `Serializer` keeps
    /// the frame, not the borrow).
    pub(crate) fn root_frame(self, name: Option<&'static str>, seq: bool) -> Result<Frame, Error> {
        if let Some(name) = name {
            self.st.variants.push((name, false));
        }
        let shape = if seq { Shape::Seq } else { Shape::Map };
        frame(self.st, self.rules, self.at, shape)
    }
}

impl<'b> ser::Serializer for Streamer<'b> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'b>;
    type SerializeTuple = Compound<'b>;
    type SerializeTupleStruct = Compound<'b>;
    type SerializeTupleVariant = Compound<'b>;
    type SerializeMap = Compound<'b>;
    type SerializeStruct = Compound<'b>;
    type SerializeStructVariant = Compound<'b>;

    fn serialize_bool(self, v: bool) -> Result<(), Error> {
        disarm();
        self.plain(&Value::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_i128(self, v: i128) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_u8(self, v: u8) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_u128(self, v: u128) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_f32(self, v: f32) -> Result<(), Error> {
        disarm();
        self.number(Number::from(f32_value(v)))
    }
    fn serialize_f64(self, v: f64) -> Result<(), Error> {
        disarm();
        self.number(Number::from(v))
    }
    fn serialize_char(self, v: char) -> Result<(), Error> {
        disarm();
        let mut buf = [0u8; 4];
        self.string(v.encode_utf8(&mut buf))
    }
    fn serialize_str(self, v: &str) -> Result<(), Error> {
        disarm();
        self.string(v)
    }
    /// A sequence of byte values (as `serde_yaml`).
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        disarm();
        let mut c = self.collection(Shape::Seq)?;
        for b in v {
            c.frame.element(c.st, c.rules, b)?;
        }
        c.frame.end(c.st, c.rules)
    }
    fn serialize_none(self) -> Result<(), Error> {
        disarm();
        self.plain(&Value::Null)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), Error> {
        disarm();
        let at = self.nested()?;
        emit(v, self.st, self.rules, at)
    }
    fn serialize_unit(self) -> Result<(), Error> {
        disarm();
        self.plain(&Value::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Error> {
        disarm();
        self.plain(&Value::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
    ) -> Result<(), Error> {
        disarm();
        self.string(name)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        disarm();
        if name == VALUE_MAGIC {
            // A `Value` is taken whole and emitted iteratively.
            if let Some(value) = take_handed_over() {
                // A handed-over scalar costs no
                // typed level, as in the build.
                if !is_scalar(&value) {
                    self.nested()?;
                }
                handed(self.st, self.rules, self.at, &value);
                crate::value::walk::drop_value_with(value, &mut self.st.drops);
                return Ok(());
            }
            // A spoofed name finds nothing handed over (see
            // `ToValue::serialize_newtype_struct`).
            let at = self.nested()?;
            return v.serialize(Streamer {
                st: self.st,
                rules: self.rules,
                at,
            });
        }
        let at = self.nested()?;
        emit(v, self.st, self.rules, at)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        disarm();
        let at = self.nested()?;
        let pending = self.st.variants.len();
        self.st.variants.push((name, false));
        let r = emit(v, self.st, self.rules, at);
        if r.is_err() {
            // A content that failed before its first call leaves its
            // variant pending; it must not wrap whatever a `Serialize`
            // impl that swallows the error writes next (Y3016 stays the
            // document's error).
            self.st.variants.truncate(pending);
        }
        r.map_err(|e| e.in_key(name))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'b>, Error> {
        disarm();
        self.collection(Shape::Seq)
    }
    fn serialize_tuple(self, _: usize) -> Result<Compound<'b>, Error> {
        disarm();
        self.collection(Shape::Seq)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Compound<'b>, Error> {
        disarm();
        self.collection(Shape::Seq)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        _: usize,
    ) -> Result<Compound<'b>, Error> {
        disarm();
        self.variant_collection(name, Shape::Seq)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'b>, Error> {
        disarm();
        self.collection(Shape::Map)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Compound<'b>, Error> {
        disarm();
        self.collection(Shape::Map)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        _: usize,
    ) -> Result<Compound<'b>, Error> {
        disarm();
        self.variant_collection(name, Shape::Map)
    }
}

/// A collection being streamed below the document root.
pub(crate) struct Compound<'b> {
    st: &'b mut DocState,
    rules: &'b Rules,
    frame: Frame,
}

macro_rules! element_compound {
    ($($trait:ident::$method:ident),* $(,)?) => {$(
        impl ser::$trait for Compound<'_> {
            type Ok = ();
            type Error = Error;
            fn $method<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
                self.frame.element(self.st, self.rules, v)
            }
            fn end(self) -> Result<(), Error> {
                self.frame.end(self.st, self.rules)
            }
        }
    )*};
}

element_compound!(
    SerializeSeq::serialize_element,
    SerializeTuple::serialize_element,
    SerializeTupleStruct::serialize_field,
    SerializeTupleVariant::serialize_field,
);

macro_rules! field_compound {
    ($($trait:ident),* $(,)?) => {$(
        impl ser::$trait for Compound<'_> {
            type Ok = ();
            type Error = Error;
            fn serialize_field<T: ?Sized + Serialize>(
                &mut self,
                name: &'static str,
                v: &T,
            ) -> Result<(), Error> {
                self.frame.field(self.st, self.rules, name, v)
            }
            fn end(self) -> Result<(), Error> {
                self.frame.end(self.st, self.rules)
            }
        }
    )*};
}

field_compound!(SerializeStruct, SerializeStructVariant);

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), Error> {
        self.frame.key(self.st, k)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.frame.value(self.st, self.rules, v)
    }
    fn end(self) -> Result<(), Error> {
        self.frame.end(self.st, self.rules)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn keys_detect_duplicates_inline_and_in_sets() {
        let mut k = Keys::default();
        let names: Vec<&'static str> =
            vec!["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
        for n in &names {
            assert!(k.insert_name(n));
        }
        assert!(k.name_set.is_some());
        for n in &names {
            assert!(!k.insert_name(n), "{n}");
        }
        let mut k = Keys::default();
        for i in 0..20u32 {
            assert!(k.insert_value(Value::from(i)).is_none());
            assert_eq!(k.value_set.is_some(), i >= 8);
        }
        for i in 0..20u32 {
            assert!(k.insert_value(Value::from(i)).is_some());
        }
        let mut k = Keys::default();
        assert!(k.insert_value(Value::from("x")).is_none());
        assert_eq!(k.insert_value(Value::from("x")), Some(Value::from("x")));
        // Collection keys compare by meaning (order-independent mappings).
        let a: Value = crate::from_str("{a: 1, b: 2}").unwrap();
        let b: Value = crate::from_str("{b: 2, a: 1}").unwrap();
        assert!(k.insert_value(a).is_none());
        assert!(k.insert_value(b).is_some());
        // Scalar and collection keys mixed, before and after the set.
        let mixed = |i: u32| {
            if i % 2 == 0 {
                Value::from(i)
            } else {
                Value::from(vec![Value::from(i)])
            }
        };
        for n in [3u32, 8, 9, 20] {
            let mut k = Keys::default();
            for i in 0..n {
                assert!(k.insert_value(mixed(i)).is_none(), "{n} {i}");
            }
            assert_eq!(k.value_set.is_some(), n > 8);
            for i in 0..n {
                assert!(k.insert_value(mixed(i)).is_some(), "{n} {i}");
            }
            // `1` and `[1]` are different keys.
            assert!(k.insert_value(Value::from(vec![Value::from(0)])).is_none());
        }
    }
}

#[cfg(test)]
mod depth_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::emit::output_rules;
    use oneyaml_core::Settings;

    /// Once a node is past `max_depth`, nothing more
    /// is appended, however much follows.
    #[test]
    fn nothing_is_written_past_max_depth() {
        let s = Settings::new().max_depth(2);
        let rules = output_rules(&s).unwrap();
        let mut doc = DocState::new(s.get_limits());
        let tail: Vec<String> = vec!["x".repeat(100); 1000];
        let value = (
            vec![vec![vec![1]]],
            &tail,
            std::collections::BTreeMap::from([(1, &tail)]),
        );
        let e = doc.document(&rules, Start::First, &value).unwrap_err();
        assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
        assert!(doc.out.len() < 16, "{:?}", doc.out);
        // A deep handed-over mapping chain is refused before any of it.
        let mut deep = Value::Null;
        for _ in 0..10_000 {
            let mut m = crate::value::Mapping::new();
            m.insert(Value::from("a"), deep);
            deep = Value::from(m);
        }
        let e = doc
            .document(&rules, Start::First, &vec![&deep])
            .unwrap_err();
        assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
        // Only the entry indicator before it.
        assert_eq!(doc.out, "- ");
    }
}
