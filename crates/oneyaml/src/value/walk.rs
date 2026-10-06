//! Iterative traversals over [`Value`] trees.
//!
//! Every operation whose cost scales with nesting depth lives here and uses
//! an explicit heap stack: teardown, clone, equality, hashing (digests),
//! debug formatting, and depth measurement. Derived impls would recurse once per
//! level and overflow the native stack on deep input.

use std::collections::hash_map::{DefaultHasher, RandomState};
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::mem;
use std::slice;
use std::sync::OnceLock;

use super::mapping::Entry;
use super::{Mapping, STRING_TAG, Sequence, Tag, TaggedValue, Value};

// ---------------------------------------------------------------- teardown

/// True when dropping `v` in place could recurse (it owns nested values).
fn needs_teardown(v: &Value) -> bool {
    match v {
        Value::Sequence(s) => !s.items.is_empty(),
        Value::Mapping(m) => !m.is_empty(),
        // Not `needs_teardown(&t.value)`: that would recurse down a tag chain.
        Value::Tagged(t) => matches!(
            t.value,
            Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_)
        ),
        _ => false,
    }
}

fn defer(v: Value, stack: &mut Vec<Value>) {
    if needs_teardown(&v) {
        stack.push(v);
    }
    // Otherwise `v` is a leaf or an empty container: dropping it here is
    // O(1) and cannot recurse.
}

/// Moves `v`'s children out (onto `stack` if they own anything), leaving
/// `v` shallow.
fn dismantle(v: &mut Value, stack: &mut Vec<Value>) {
    match v {
        Value::Sequence(s) => {
            for c in mem::take(&mut s.items) {
                defer(c, stack);
            }
        }
        Value::Mapping(m) => {
            for e in m.take_entries() {
                defer(e.key, stack);
                defer(e.value, stack);
            }
        }
        Value::Tagged(t) => defer(mem::replace(&mut t.value, Value::Null), stack),
        _ => {}
    }
}

fn run_teardown(mut stack: Vec<Value>) {
    while let Some(mut v) = stack.pop() {
        dismantle(&mut v, &mut stack);
        // `v` is now shallow; its own `Drop` finds nothing to do.
    }
}

/// Drops sequence items without recursion.
pub(crate) fn drop_items(items: Vec<Value>) {
    let mut stack = Vec::new();
    for c in items {
        defer(c, &mut stack);
    }
    run_teardown(stack);
}

/// Drops mapping entries without recursion.
pub(crate) fn drop_all(entries: Vec<Entry>) {
    let mut stack = Vec::new();
    for e in entries {
        defer(e.key, &mut stack);
        defer(e.value, &mut stack);
    }
    run_teardown(stack);
}

/// [`drop_value`] on a caller's stack (emptied first; the streaming
/// serializer drops every handed-over `Value` on one stack per document).
pub(crate) fn drop_value_with(v: Value, stack: &mut Vec<Value>) {
    stack.clear();
    defer(v, stack);
    while let Some(mut v) = stack.pop() {
        dismantle(&mut v, stack);
    }
}

/// Drops one value without recursion.
pub(crate) fn drop_value(v: Value) {
    let mut stack = Vec::new();
    defer(v, &mut stack);
    run_teardown(stack);
}

// ------------------------------------------------------------------- clone

/// A collection being cloned. Every frame on the stack waits for exactly one
/// finished child, so delivering a value is always meaningful: there is no
/// state in which a clone could be dropped or misplaced (never a panic, by
/// construction, no `debug_assert!` fallbacks).
enum CloneFrame<'a> {
    /// Waiting for the next item.
    Seq {
        rest: slice::Iter<'a, Value>,
        out: Vec<Value>,
    },
    /// Waiting for `entry`'s key.
    Key {
        like: &'a Mapping,
        entry: &'a Entry,
        rest: slice::Iter<'a, Entry>,
        out: Vec<Entry>,
    },
    /// Waiting for `entry`'s value; its key is cloned.
    Val {
        like: &'a Mapping,
        entry: &'a Entry,
        key: Value,
        rest: slice::Iter<'a, Entry>,
        out: Vec<Entry>,
    },
    /// Waiting for the tagged content.
    Tag(&'a Tag),
}

/// Deep-clones `root` without recursion.
pub(crate) fn clone_value(root: &Value) -> Value {
    let mut stack: Vec<CloneFrame<'_>> = Vec::new();
    let mut visit = root;
    loop {
        // Descend until a finished value exists.
        let mut done = match visit {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Number(n) => Value::Number(n.clone()),
            Value::String(s) => Value::String(s.clone()),
            Value::Sequence(s) => {
                let mut rest = s.items.iter();
                match rest.next() {
                    Some(first) => {
                        stack.push(CloneFrame::Seq {
                            rest,
                            out: Vec::with_capacity(s.items.len()),
                        });
                        visit = first;
                        continue;
                    }
                    None => Value::Sequence(Sequence::new()),
                }
            }
            Value::Mapping(m) => {
                let mut rest = m.entries.iter();
                match rest.next() {
                    Some(entry) => {
                        stack.push(CloneFrame::Key {
                            like: m,
                            entry,
                            rest,
                            out: Vec::with_capacity(m.len()),
                        });
                        visit = &entry.key;
                        continue;
                    }
                    None => Value::Mapping(Mapping::from_cloned(Vec::new(), m)),
                }
            }
            Value::Tagged(t) => {
                stack.push(CloneFrame::Tag(&t.tag));
                visit = &t.value;
                continue;
            }
        };
        // Deliver upwards until a frame asks for another child.
        loop {
            match stack.pop() {
                None => return done,
                Some(CloneFrame::Seq { mut rest, mut out }) => {
                    out.push(done);
                    if let Some(next) = rest.next() {
                        stack.push(CloneFrame::Seq { rest, out });
                        visit = next;
                        break;
                    }
                    done = Value::Sequence(Sequence { items: out });
                }
                Some(CloneFrame::Key {
                    like,
                    entry,
                    rest,
                    out,
                }) => {
                    stack.push(CloneFrame::Val {
                        like,
                        entry,
                        key: done,
                        rest,
                        out,
                    });
                    visit = &entry.value;
                    break;
                }
                Some(CloneFrame::Val {
                    like,
                    entry,
                    key,
                    mut rest,
                    mut out,
                }) => {
                    out.push(Entry {
                        digest: entry.digest,
                        key,
                        value: done,
                    });
                    if let Some(next) = rest.next() {
                        stack.push(CloneFrame::Key {
                            like,
                            entry: next,
                            rest,
                            out,
                        });
                        visit = &next.key;
                        break;
                    }
                    done = Value::Mapping(Mapping::from_cloned(out, like));
                }
                Some(CloneFrame::Tag(tag)) => {
                    done = Value::Tagged(Box::new(TaggedValue {
                        tag: tag.clone(),
                        value: done,
                    }));
                }
            }
        }
    }
}

// ---------------------------------------------------------------- equality

/// One obligation of an equality check.
enum Task<'a> {
    /// The two values must be equal.
    Pair(&'a Value, &'a Value),
    /// `key` must equal the key of one of `cands` (entries of the other
    /// mapping with the same key digest), and `val` that entry's value.
    OneOf {
        key: &'a Value,
        val: &'a Value,
        cands: Vec<&'a Entry>,
    },
}

/// A conjunction of tasks. A context with `alt` is trying `cands[at]` for
/// a [`Task::OneOf`]; when it fails, the next candidate is tried.
struct Ctx<'a> {
    pending: Vec<Task<'a>>,
    alt: Option<Alt<'a>>,
}

struct Alt<'a> {
    key: &'a Value,
    val: &'a Value,
    cands: Vec<&'a Entry>,
    at: usize,
}

impl<'a> Alt<'a> {
    /// The key comparison for the current candidate.
    fn attempt(&self) -> Option<Task<'a>> {
        self.cands
            .get(self.at)
            .map(|c| Task::Pair(self.key, &c.key))
    }
}

/// Pushes the checks that make `x` and `y` equal regardless of entry order;
/// `false` if they cannot be. Each `x` entry is matched through `y`'s index
/// by its cached key digest: equal keys have equal digests, so the match is
/// among the candidates. Keys are unique within a mapping and the lengths
/// agree, so matching every `x` entry makes the matching a bijection.
fn push_map_tasks<'a>(x: &'a Mapping, y: &'a Mapping, out: &mut Vec<Task<'a>>) -> bool {
    if x.len() != y.len() {
        return false;
    }
    for e in &x.entries {
        let mut cands = y
            .positions(e.digest)
            .iter()
            .filter_map(|i| y.entries.get(*i))
            .filter(|f| f.digest == e.digest);
        match (cands.next(), cands.next()) {
            (None, _) => return false,
            (Some(f), None) => {
                out.push(Task::Pair(&e.value, &f.value));
                out.push(Task::Pair(&e.key, &f.key));
            }
            (Some(f), Some(g)) => {
                let mut all = vec![f, g];
                all.extend(cands);
                out.push(Task::OneOf {
                    key: &e.key,
                    val: &e.value,
                    cands: all,
                });
            }
        }
    }
    true
}

fn push_seq_tasks<'a>(x: &'a Sequence, y: &'a Sequence, out: &mut Vec<Task<'a>>) -> bool {
    if x.items.len() != y.items.len() {
        return false;
    }
    out.extend(x.items.iter().zip(&y.items).map(|(a, b)| Task::Pair(a, b)));
    true
}

/// Compares one level; children become new tasks.
fn eq_shallow<'a>(a: &'a Value, b: &'a Value, out: &mut Vec<Task<'a>>) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Sequence(x), Value::Sequence(y)) => push_seq_tasks(x, y, out),
        (Value::Mapping(x), Value::Mapping(y)) => push_map_tasks(x, y, out),
        (Value::Tagged(x), Value::Tagged(y)) => {
            out.push(Task::Pair(&x.value, &y.value));
            x.tag == y.tag
        }
        _ => false,
    }
}

/// Runs the tasks of `root` to completion: an explicit stack of contexts,
/// with backtracking only across key-digest collisions (no
/// recursion, however keys nest).
fn run_eq(root: Vec<Task<'_>>) -> bool {
    let mut ctxs = vec![Ctx {
        pending: root,
        alt: None,
    }];
    loop {
        let Some(top) = ctxs.last_mut() else {
            return false;
        };
        let ok = match top.pending.pop() {
            Some(Task::Pair(a, b)) => eq_shallow(a, b, &mut top.pending),
            Some(Task::OneOf { key, val, cands }) => {
                let alt = Alt {
                    key,
                    val,
                    cands,
                    at: 0,
                };
                let first = alt.attempt();
                ctxs.push(Ctx {
                    pending: first.into_iter().collect(),
                    alt: Some(alt),
                });
                continue;
            }
            None => {
                // The context holds: the root means equal; a candidate
                // context means its key matched, so its value must too.
                let Some(Ctx { alt, .. }) = ctxs.pop() else {
                    return false;
                };
                let Some(alt) = alt else {
                    return true;
                };
                let (Some(parent), Some(c)) = (ctxs.last_mut(), alt.cands.get(alt.at)) else {
                    return false;
                };
                parent.pending.push(Task::Pair(alt.val, &c.value));
                continue;
            }
        };
        if ok {
            continue;
        }
        // Failure: retry the innermost context with candidates left, or
        // fail its enclosing context in turn.
        loop {
            let Some(mut ctx) = ctxs.pop() else {
                return false;
            };
            let Some(alt) = ctx.alt.as_mut() else {
                return false;
            };
            alt.at = alt.at.saturating_add(1);
            if let Some(task) = alt.attempt() {
                ctx.pending.clear();
                ctx.pending.push(task);
                ctxs.push(ctx);
                break;
            }
        }
    }
}

/// Structural equality without recursion; mappings compare order-free.
pub(crate) fn eq(a: &Value, b: &Value) -> bool {
    let mut tasks = Vec::new();
    // Equal scalars leave no task, and no context stack
    // is allocated for them (a scalar key confirmed after a digest match,
    // `KeyMap`'s small keys, `Value` lookups).
    eq_shallow(a, b, &mut tasks) && (tasks.is_empty() || run_eq(tasks))
}

pub(crate) fn eq_sequences(a: &Sequence, b: &Sequence) -> bool {
    let mut tasks = Vec::new();
    push_seq_tasks(a, b, &mut tasks) && run_eq(tasks)
}

pub(crate) fn eq_mappings(a: &Mapping, b: &Mapping) -> bool {
    let mut tasks = Vec::new();
    push_map_tasks(a, b, &mut tasks) && run_eq(tasks)
}

// ----------------------------------------------------------------- hashing

// Discriminants written before each node.
const NULL_TAG: u8 = 0;
const BOOL_TAG: u8 = 1;
const NUMBER_TAG: u8 = 2;
const SEQ_TAG: u8 = 4;
const MAP_TAG: u8 = 5;
const TAGGED_TAG: u8 = 6;

/// A hasher keyed once per process with random keys (hash-flooding
/// resistance): digests are comparable within the process (cached
/// key digests stay valid across mappings and clones) and unpredictable
/// from outside it.
fn hasher() -> DefaultHasher {
    static SEED: OnceLock<RandomState> = OnceLock::new();
    SEED.get_or_init(RandomState::new).build_hasher()
}

/// The digest of `Value::String(s)`.
pub(crate) fn str_digest(s: &str) -> u64 {
    let mut h = hasher();
    h.write_u8(STRING_TAG);
    s.hash(&mut h);
    h.finish()
}

/// One mapping entry's contribution, summed with `wrapping_add` (a
/// commutative combination, so entry order does not matter).
fn entry_digest(key: u64, value: u64) -> u64 {
    let mut h = hasher();
    h.write_u64(key);
    h.write_u64(value);
    h.finish()
}

fn map_digest(len: usize, sum: u64) -> u64 {
    let mut h = hasher();
    h.write_u8(MAP_TAG);
    h.write_usize(len);
    h.write_u64(sum);
    h.finish()
}

/// A collection whose digest is being computed; each waits for one child
/// digest.
enum DigestFrame<'a> {
    Seq {
        h: DefaultHasher,
        rest: slice::Iter<'a, Value>,
    },
    /// Waiting for the value of the entry whose key digest is `key`.
    Map {
        len: usize,
        sum: u64,
        key: u64,
        rest: slice::Iter<'a, Entry>,
    },
    Tag(&'a Tag),
}

enum Next<'a> {
    Done(u64),
    Visit(&'a Value),
}

fn open_seq<'a>(s: &'a Sequence, stack: &mut Vec<DigestFrame<'a>>) -> Next<'a> {
    let mut h = hasher();
    h.write_u8(SEQ_TAG);
    h.write_usize(s.items.len());
    let mut rest = s.items.iter();
    match rest.next() {
        Some(first) => {
            stack.push(DigestFrame::Seq { h, rest });
            Next::Visit(first)
        }
        None => Next::Done(h.finish()),
    }
}

fn open_map<'a>(m: &'a Mapping, stack: &mut Vec<DigestFrame<'a>>) -> Next<'a> {
    let mut rest = m.entries.iter();
    match rest.next() {
        // Keys are never walked: their digests are cached in the entries.
        Some(e) => {
            stack.push(DigestFrame::Map {
                len: m.len(),
                sum: 0,
                key: e.digest,
                rest,
            });
            Next::Visit(&e.value)
        }
        None => Next::Done(map_digest(0, 0)),
    }
}

fn open<'a>(v: &'a Value, stack: &mut Vec<DigestFrame<'a>>) -> Next<'a> {
    let mut h = hasher();
    match v {
        Value::Null => h.write_u8(NULL_TAG),
        Value::Bool(b) => {
            h.write_u8(BOOL_TAG);
            b.hash(&mut h);
        }
        Value::Number(n) => {
            h.write_u8(NUMBER_TAG);
            n.hash(&mut h);
        }
        Value::String(s) => return Next::Done(str_digest(s)),
        Value::Sequence(s) => return open_seq(s, stack),
        Value::Mapping(m) => return open_map(m, stack),
        Value::Tagged(t) => {
            stack.push(DigestFrame::Tag(&t.tag));
            return Next::Visit(&t.value);
        }
    }
    Next::Done(h.finish())
}

fn run_digest<'a>(mut next: Next<'a>, stack: &mut Vec<DigestFrame<'a>>) -> u64 {
    loop {
        let mut d = match next {
            Next::Visit(v) => {
                next = open(v, stack);
                continue;
            }
            Next::Done(d) => d,
        };
        loop {
            match stack.pop() {
                None => return d,
                Some(DigestFrame::Seq { mut h, mut rest }) => {
                    h.write_u64(d);
                    if let Some(c) = rest.next() {
                        stack.push(DigestFrame::Seq { h, rest });
                        next = Next::Visit(c);
                        break;
                    }
                    d = h.finish();
                }
                Some(DigestFrame::Map {
                    len,
                    sum,
                    key,
                    mut rest,
                }) => {
                    let sum = sum.wrapping_add(entry_digest(key, d));
                    if let Some(e) = rest.next() {
                        stack.push(DigestFrame::Map {
                            len,
                            sum,
                            key: e.digest,
                            rest,
                        });
                        next = Next::Visit(&e.value);
                        break;
                    }
                    d = map_digest(len, sum);
                }
                Some(DigestFrame::Tag(tag)) => {
                    let mut h = hasher();
                    h.write_u8(TAGGED_TAG);
                    tag.hash(&mut h);
                    h.write_u64(d);
                    d = h.finish();
                }
            }
        }
    }
}

/// The structural digest of `v`, consistent with [`eq`] (equal values have
/// equal digests; mapping entry order does not matter). Iterative;
/// O(size of `v`) excluding the keys of nested mappings, whose digests are
/// cached. `Value`'s `Hash` writes exactly this.
pub(crate) fn digest(v: &Value) -> u64 {
    let mut stack = Vec::new();
    let first = open(v, &mut stack);
    run_digest(first, &mut stack)
}

pub(crate) fn digest_sequence(s: &Sequence) -> u64 {
    let mut stack = Vec::new();
    let first = open_seq(s, &mut stack);
    run_digest(first, &mut stack)
}

pub(crate) fn digest_mapping(m: &Mapping) -> u64 {
    let mut stack = Vec::new();
    let first = open_map(m, &mut stack);
    run_digest(first, &mut stack)
}

// ------------------------------------------------------------------- debug

enum Tok<'a> {
    V(&'a Value),
    S(&'static str),
}

fn push_seq_toks<'a>(s: &'a Sequence, stack: &mut Vec<Tok<'a>>) {
    stack.push(Tok::S("]"));
    for (i, item) in s.items.iter().enumerate().rev() {
        stack.push(Tok::V(item));
        if i != 0 {
            stack.push(Tok::S(", "));
        }
    }
}

fn push_map_toks<'a>(m: &'a Mapping, stack: &mut Vec<Tok<'a>>) {
    stack.push(Tok::S("}"));
    for (i, e) in m.entries.iter().enumerate().rev() {
        stack.push(Tok::V(&e.value));
        stack.push(Tok::S(": "));
        stack.push(Tok::V(&e.key));
        if i != 0 {
            stack.push(Tok::S(", "));
        }
    }
}

/// The tag prefix of an opaque key identity standing for an alias of a
/// collection anchor, read without expansion: `…alias/{serial}/{name}`.
pub(crate) const OPAQUE_ALIAS: &str = "tag:oneyaml.invalid,2026:alias/";
/// The tag prefix of an opaque key identity standing for a mapping whose
/// merged entries only expansion reveals: `…merged/{serial}`.
pub(crate) const OPAQUE_MERGED: &str = "tag:oneyaml.invalid,2026:merged/";

/// How an opaque key identity reads in messages and paths: the
/// alias as written (`*n`), or a mapping that merges one.
fn opaque_text(tag: &Tag) -> Option<&str> {
    // (Our opaque identities are built in one piece.)
    let t = tag.parts().as_single()?;
    if let Some(rest) = t.strip_prefix(OPAQUE_ALIAS) {
        return rest.split_once('/').map(|(_, name)| name);
    }
    t.starts_with(OPAQUE_MERGED).then_some("")
}

fn run_debug(stack: Vec<Tok<'_>>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    run_debug_as(stack, f, false)
}

/// `keys`: opaque key identities are shown as what the source says
/// (`*n`, `Mapping {<<: …}`), never as their internal tags.
fn run_debug_as(mut stack: Vec<Tok<'_>>, f: &mut fmt::Formatter<'_>, keys: bool) -> fmt::Result {
    while let Some(tok) = stack.pop() {
        match tok {
            Tok::S(s) => f.write_str(s)?,
            Tok::V(Value::Null) => f.write_str("Null")?,
            Tok::V(Value::Bool(b)) => write!(f, "Bool({b})")?,
            Tok::V(Value::Number(n)) => write!(f, "Number({n})")?,
            Tok::V(Value::String(s)) => write!(f, "String({s:?})")?,
            Tok::V(Value::Sequence(s)) => {
                f.write_str("Sequence [")?;
                push_seq_toks(s, &mut stack);
            }
            Tok::V(Value::Mapping(m)) => {
                f.write_str("Mapping {")?;
                push_map_toks(m, &mut stack);
            }
            Tok::V(Value::Tagged(t)) if keys && opaque_text(&t.tag).is_some() => {
                match opaque_text(&t.tag) {
                    Some(name) if !name.is_empty() => write!(f, "*{name}")?,
                    _ => f.write_str("Mapping {<<: …}")?,
                }
            }
            Tok::V(Value::Tagged(t)) => {
                write!(f, "Tagged({}, ", t.tag)?;
                stack.push(Tok::S(")"));
                stack.push(Tok::V(&t.value));
            }
        }
    }
    Ok(())
}

/// `Debug` without recursion (the `{:#?}` flag is ignored).
pub(crate) fn debug_value(v: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    run_debug(vec![Tok::V(v)], f)
}

/// A key as messages and paths show it: `Debug`, with opaque key
/// identities as the source wrote them. Iterative.
pub(crate) struct KeyDebug<'a>(pub(crate) &'a Value);

impl fmt::Display for KeyDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        run_debug_as(vec![Tok::V(self.0)], f, true)
    }
}

pub(crate) fn debug_sequence(s: &Sequence, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("[")?;
    let mut stack = Vec::new();
    push_seq_toks(s, &mut stack);
    run_debug(stack, f)
}

pub(crate) fn debug_mapping(m: &Mapping, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("{")?;
    let mut stack = Vec::new();
    push_map_toks(m, &mut stack);
    run_debug(stack, f)
}

// ------------------------------------------------------------------- depth

/// The collection nesting depth of `v` (a scalar is 0, a top-level
/// collection 1; tags add nothing), stopping early once it exceeds `cap`.
pub(crate) fn depth(v: &Value, cap: usize) -> usize {
    depth_with(v, cap, &mut Vec::new())
}

/// [`depth`] on a caller's stack (emptied first, reused by the
/// streaming serializer).
pub(crate) fn depth_with<'a>(
    v: &'a Value,
    cap: usize,
    stack: &mut Vec<(&'a Value, usize)>,
) -> usize {
    let mut max = 0usize;
    stack.clear();
    stack.push((v, 0));
    while let Some((v, d)) = stack.pop() {
        match v {
            Value::Sequence(s) => {
                let d = d.saturating_add(1);
                max = max.max(d);
                stack.extend(s.items.iter().map(|c| (c, d)));
            }
            Value::Mapping(m) => {
                let d = d.saturating_add(1);
                max = max.max(d);
                for e in &m.entries {
                    stack.push((&e.key, d));
                    stack.push((&e.value, d));
                }
            }
            Value::Tagged(t) => stack.push((&t.value, d)),
            _ => {}
        }
        if max > cap {
            break;
        }
    }
    max
}

/// The depth serde recurses through when serializing `v`: every
/// collection **and every tag** counts one level, since a tag is
/// serialized as a one-entry map (`{tag: value}`) and costs a native call
/// frame like a collection does. Stops early once it exceeds `cap`.
pub(crate) fn serde_depth(v: &Value, cap: usize) -> usize {
    let mut max = 0usize;
    let mut stack: Vec<(&Value, usize)> = vec![(v, 0)];
    while let Some((v, d)) = stack.pop() {
        let d = match v {
            Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => d.saturating_add(1),
            _ => d,
        };
        max = max.max(d);
        match v {
            Value::Sequence(s) => stack.extend(s.items.iter().map(|c| (c, d))),
            Value::Mapping(m) => {
                for e in &m.entries {
                    stack.push((&e.key, d));
                    stack.push((&e.value, d));
                }
            }
            Value::Tagged(t) => stack.push((&t.value, d)),
            _ => {}
        }
        if max > cap {
            break;
        }
    }
    max
}
