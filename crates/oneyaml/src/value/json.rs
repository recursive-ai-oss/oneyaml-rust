//! Conversions between [`Value`] and `serde_json::Value` (feature `serde_json`,
//! off by default: no runtime dependency unless asked for). The lossiness rules
//! are on [`Value::to_json_with`] (public docs).

use oneyaml_core::{Limits, Settings, Span};

use super::{Mapping, Number, Sequence, Value};
use crate::construct::Tag as EventTag;
use crate::construct::keys::key_text;
use crate::construct::scalar::resolve;
use crate::error::Error;
use crate::resolve::{Resolved, Resolver};

type Json = serde_json::Value;

/// JSON → YAML (see the lossiness rules on [`Value::to_json_with`]). JSON
/// numbers are read as YAML 1.2 Core numbers by the one resolver, exact
/// at any width; objects keep serde_json's iteration order. Iterative: any
/// depth is safe.
///
/// It fails only for a number whose text overflows a float (`1e400`),
/// which only serde_json's `arbitrary_precision` can hold: refused
/// (`Y3006`, with its path) rather than read as `.inf`.
///
/// ```
/// let json = serde_json::json!({"name": "web", "replicas": 3, "ports": [80, 443]});
/// let yaml = oneyaml::Value::try_from(json).unwrap();
/// assert_eq!(oneyaml::to_string(&yaml).unwrap(), "name: web\nports:\n  - 80\n  - 443\nreplicas: 3\n");
/// ```
#[cfg_attr(docsrs, doc(cfg(feature = "serde_json")))]
impl TryFrom<Json> for Value {
    type Error = Error;

    fn try_from(json: Json) -> Result<Self, Error> {
        from_json(json)
    }
}

/// YAML → JSON with default [`Limits`]; see [`Value::to_json_with`].
///
/// ```
/// let v: oneyaml::Value = oneyaml::from_str("name: web\nports: [80, 443]\n").unwrap();
/// let json = serde_json::Value::try_from(&v).unwrap();
/// assert_eq!(json, serde_json::json!({"name": "web", "ports": [80, 443]}));
///
/// let tagged: oneyaml::Value = oneyaml::from_str("color: !rgb 0f0\n").unwrap();
/// let e = serde_json::Value::try_from(&tagged).unwrap_err();
/// assert_eq!(e.code(), oneyaml::code::TYPE_MISMATCH);
/// assert_eq!(e.to_string(), "tagged value `!rgb` has no JSON form at color");
/// ```
#[cfg_attr(docsrs, doc(cfg(feature = "serde_json")))]
impl TryFrom<&Value> for Json {
    type Error = Error;

    fn try_from(value: &Value) -> Result<Self, Error> {
        to_json(value, &Limits::default())
    }
}

/// YAML → JSON with default [`Limits`]; see [`Value::to_json_with`].
///
/// ```
/// let v: oneyaml::Value = oneyaml::from_str("[1, .nan]").unwrap();
/// let e = serde_json::Value::try_from(v).unwrap_err();
/// assert_eq!(e.code(), oneyaml::code::INVALID_VALUE);
/// assert_eq!(e.path().unwrap().to_string(), "[1]");
/// ```
#[cfg_attr(docsrs, doc(cfg(feature = "serde_json")))]
impl TryFrom<Value> for Json {
    type Error = Error;

    fn try_from(value: Value) -> Result<Self, Error> {
        to_json(&value, &Limits::default())
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "serde_json")))]
impl Value {
    /// This value as a `serde_json::Value` (feature `serde_json`), with
    /// explicit [`Settings`] (only `Limits::max_depth` applies). What has no
    /// JSON form is an error, never a silent coercion.
    ///
    /// # Lossiness
    ///
    /// **JSON → YAML** (`TryFrom<serde_json::Value>`): every JSON value has
    /// one YAML meaning. JSON numbers are read by the one resolver as YAML
    /// 1.2 Core numbers (every JSON number is a Core `!!int` or `!!float`),
    /// so integers of any width (serde_json's `arbitrary_precision`) stay
    /// exact; objects keep serde_json's iteration order (sorted by key, or
    /// insertion order with serde_json's `preserve_order` feature). What
    /// YAML receives is what serde_json holds, and serde_json's features
    /// change that for two inputs:
    ///
    /// | JSON text | without `arbitrary_precision` | with it |
    /// |-----------|--------------------------------|---------|
    /// | `-0`      | float `-0.0` (serde_json parses it as `-0.0`) | integer `0` (Core reads the text `-0`) |
    /// | `1e400`   | not held (serde_json rejects it) | refused, `Y3006` (never `.inf`) |
    ///
    /// **YAML → JSON is fallible** (`TryFrom<&Value>`, `to_json_with`):
    /// what has no JSON form is an error with the path of the offending node,
    /// never a silent coercion.
    ///
    /// | YAML                                           | JSON                          |
    /// |------------------------------------------------|-------------------------------|
    /// | `Null`, `Bool`, `String`                       | the same                      |
    /// | integer within `i64`/`u64`                     | number                        |
    /// | integer outside `i64`/`u64`                    | `Y3001` (`INTEGER_OUT_OF_RANGE`) |
    /// | finite float (`-0.0` included)                 | number                        |
    /// | `.nan`, `.inf`, `-.inf`                        | `Y3006` (`INVALID_VALUE`)     |
    /// | `Sequence`                                     | array                         |
    /// | `Mapping` with string keys                     | object (see ordering above)   |
    /// | a non-string key (`1: x`, `? [a]`, `null: x`)  | `Y3000` (`TYPE_MISMATCH`)     |
    /// | `Tagged` (`!color red`)                        | `Y3000` (`TYPE_MISMATCH`)     |
    /// | nesting deeper than `Limits::max_depth`        | `Y2005` (`VALUE_DEPTH_EXCEEDED`) |
    /// | nesting deeper than 128 levels (fixed)         | `Y2008` (`JSON_DEPTH_EXCEEDED`) |
    ///
    /// Duplicate keys cannot arise: a `Mapping` has none, and only string keys
    /// convert. YAML → JSON refuses integers beyond 64 bits even with
    /// serde_json's `arbitrary_precision`, so that direction never depends
    /// on another crate's feature choice.
    ///
    /// `serde_json::to_value(&value)` (serde's generic path, as with
    /// `serde_yaml`) remains available and is lossy by design: it writes a tag
    /// as a one-entry object `{"!tag": content}`, stringifies scalar keys, and
    /// writes non-finite floats as `null`.
    ///
    /// Both directions are iterative. A `serde_json::Value` drops
    /// recursively (serde_json's own `Drop`), so the one built here is at
    /// most 128 levels deep, even under `Limits::unlimited()` (the
    /// same bound as serde_json's parser and `Serialize for Value`).
    ///
    /// ```
    /// use oneyaml::{Limits, Settings};
    ///
    /// let v: oneyaml::Value = oneyaml::from_str("a: {b: [1.5, true, null]}").unwrap();
    /// let json = v.to_json_with(&Settings::new()).unwrap();
    /// assert_eq!(json, serde_json::json!({"a": {"b": [1.5, true, null]}}));
    ///
    /// let shallow = Settings::new().limits(Limits::default().max_depth(2));
    /// let e = v.to_json_with(&shallow).unwrap_err();
    /// assert_eq!(e.code(), oneyaml::code::VALUE_DEPTH_EXCEEDED);
    ///
    /// let wide: oneyaml::Value = oneyaml::from_str("n: 18446744073709551616").unwrap();
    /// let e = wide.to_json_with(&Settings::new()).unwrap_err();
    /// assert_eq!(e.code(), oneyaml::code::INTEGER_OUT_OF_RANGE);
    /// assert_eq!(e.path().unwrap().to_string(), "n");
    /// ```
    ///
    /// # Errors
    ///
    /// `Y3001` for an integer outside `i64`/`u64`, `Y3006` for a
    /// non-finite float, `Y3000` for a non-string mapping key or a tagged
    /// value, each with the path of the offending node (a `Value` has no
    /// spans); `Y2005` when the value nests deeper than `max_depth`;
    /// `Y2008` deeper than 128 levels.
    pub fn to_json_with(&self, settings: &Settings) -> Result<Json, Error> {
        to_json(self, settings.get_limits())
    }
}

/// The deepest `serde_json::Value` built here, whatever
/// `Limits::max_depth` says: serde_json's `Value` drops recursively, so an
/// unbounded depth could overflow the stack of whoever drops the result.
/// 128 is serde_json's own parser limit and `Serialize for Value`'s bound
/// for serializers that recurse (the default `max_typed_depth`).
pub(crate) const JSON_MAX_DEPTH: usize = 128;

/// The `Y2008` error of [`JSON_MAX_DEPTH`] (its own limit code).
fn too_deep_for_json() -> Error {
    Error::new(
        oneyaml_core::code::JSON_DEPTH_EXCEEDED,
        format!("JSON output nests deeper than {JSON_MAX_DEPTH} levels"),
        None,
    )
    .with_help(format!(
        "the cap of {JSON_MAX_DEPTH} levels is fixed (serde_json's recursion limit; its Value \
         drops recursively) and no Limits setting raises it; for deeper data, serialize the \
         Value itself (`Value::serialize`, e.g. `oneyaml::to_string`) instead of building a \
         serde_json::Value"
    ))
}

/// A collection being converted, with the position being filled.
enum Frame<'a> {
    Seq {
        items: std::slice::Iter<'a, Value>,
        out: Vec<Json>,
        index: usize,
    },
    Map {
        entries: super::MappingIter<'a>,
        out: serde_json::Map<String, Json>,
        /// The key of the entry whose value is being converted.
        key: String,
    },
}

impl Frame<'_> {
    fn finish(self) -> Json {
        match self {
            Frame::Seq { out, .. } => Json::Array(out),
            Frame::Map { out, .. } => Json::Object(out),
        }
    }

    fn deliver(&mut self, j: Json) {
        match self {
            Frame::Seq { out, .. } => out.push(j),
            Frame::Map { out, key, .. } => {
                out.insert(std::mem::take(key), j);
            }
        }
    }
}

/// Where a conversion failed: the error with the path of the frames
/// (outermost first), each at the child it was converting.
fn located<'f, 'a: 'f>(e: Error, frames: impl DoubleEndedIterator<Item = &'f Frame<'a>>) -> Error {
    let mut e = e;
    // Innermost first (`Error` builds paths while unwinding).
    for frame in frames.rev() {
        e = match frame {
            Frame::Seq { index, .. } => e.in_index(index.saturating_sub(1)),
            Frame::Map { key, .. } => e.in_key(key),
        };
    }
    e.finish(None, Vec::new())
}

fn number_to_json(n: &Number) -> Result<Json, Error> {
    if let Some(f) = n.as_float() {
        return serde_json::Number::from_f64(f)
            .map(Json::Number)
            .ok_or_else(|| {
                Error::new(
                    oneyaml_core::code::INVALID_VALUE,
                    format!("float {n} has no JSON form (JSON numbers are finite)"),
                    None,
                )
            });
    }
    if let Some(u) = n.as_u64() {
        return Ok(Json::Number(u.into()));
    }
    if let Some(i) = n.as_i64() {
        return Ok(Json::Number(i.into()));
    }
    Err(Error::new(
        oneyaml_core::code::INTEGER_OUT_OF_RANGE,
        format!("integer {n} is out of range for a JSON number (i64 or u64)"),
        None,
    ))
}

/// What a leaf converts to, or the collection to open.
enum Step<'a> {
    Leaf(Json),
    Open(Frame<'a>),
}

fn step(v: &Value) -> Result<Step<'_>, Error> {
    Ok(Step::Leaf(match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => number_to_json(n)?,
        Value::String(s) => Json::String(s.clone()),
        Value::Sequence(q) => {
            return Ok(Step::Open(Frame::Seq {
                items: q.iter(),
                out: Vec::with_capacity(q.len()),
                index: 0,
            }));
        }
        Value::Mapping(m) => {
            return Ok(Step::Open(Frame::Map {
                entries: m.iter(),
                out: serde_json::Map::new(),
                key: String::new(),
            }));
        }
        Value::Tagged(t) => {
            return Err(Error::new(
                oneyaml_core::code::TYPE_MISMATCH,
                format!("tagged value `{}` has no JSON form", t.tag),
                None,
            ));
        }
    }))
}

/// The next child of `top` (advancing its position), `Ok(None)` when it
/// has no more, or the error of a key with no JSON form.
fn next_child<'a>(top: &mut Frame<'a>) -> Result<Option<&'a Value>, Error> {
    match top {
        Frame::Seq { items, index, .. } => Ok(items.next().inspect(|_| {
            *index = index.saturating_add(1);
        })),
        Frame::Map { entries, key, .. } => match entries.next() {
            None => Ok(None),
            Some((Value::String(s), v)) => {
                key.clone_from(s);
                Ok(Some(v))
            }
            Some((other, _)) => Err(Error::new(
                oneyaml_core::code::TYPE_MISMATCH,
                format!(
                    "mapping key `{}` has no JSON form (JSON object keys are strings)",
                    key_text(other)
                ),
                None,
            )),
        },
    }
}

/// An explicit stack of open collections: `top` is the innermost,
/// `outer` the ones around it, so there is always a collection to fill
/// and the root is returned when `top` closes with no `outer` left.
fn to_json(root: &Value, limits: &Limits) -> Result<Json, Error> {
    if let Err(e) = root.check_depth(limits) {
        return Err(e.into_error().finish(None, Vec::new()));
    }
    // The result's depth is bounded whatever `max_depth` allows
    // (`Limits::unlimited()`), checked before anything is built.
    if super::walk::depth(root, JSON_MAX_DEPTH) > JSON_MAX_DEPTH {
        return Err(too_deep_for_json().finish(None, Vec::new()));
    }
    let mut top = match step(root) {
        Ok(Step::Leaf(j)) => return Ok(j),
        Ok(Step::Open(f)) => f,
        Err(e) => return Err(e.finish(None, Vec::new())),
    };
    let mut outer: Vec<Frame<'_>> = Vec::new();
    loop {
        let child = match next_child(&mut top) {
            Ok(c) => c,
            // The key's error is the mapping's: the path of `outer` only.
            Err(e) => return Err(located(e, outer.iter())),
        };
        match child {
            Some(v) => match step(v) {
                Ok(Step::Leaf(j)) => top.deliver(j),
                Ok(Step::Open(f)) => outer.push(std::mem::replace(&mut top, f)),
                Err(e) => return Err(located(e, outer.iter().chain(std::iter::once(&top)))),
            },
            None => {
                let done = match outer.pop() {
                    Some(parent) => std::mem::replace(&mut top, parent).finish(),
                    None => return Ok(top.finish()),
                };
                top.deliver(done);
            }
        }
    }
}

/// A JSON number as the Core schema reads its text (the one
/// resolver; exact at any width).
///
/// Why the resolver always gives a number here: serde_json's `Number`
/// displays an `u64`/`i64` as decimal digits with an optional `-` (the fast
/// paths above take those anyway), a finite `f64` in the shortest
/// round-trip form (`1.5`, `1e300`, `-0.0`, `5e-324`; serde_json never
/// holds a non-finite float), and, with `arbitrary_precision`, the
/// number's validated JSON text. Every one matches JSON's grammar
/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?`, a subset of YAML 1.2
/// Core's `[-+]?[0-9]+` (int) and
/// `[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?` (float), and Core
/// resolves a plain scalar matching either to a number, never an error
/// (§10.3.2; `json_number_texts_are_core_numbers` checks it). Should that
/// ever fail, the result is `Err(Internal)`, never a made-up value.
///
/// The one refusal: a text whose float overflows (`1e400`, held only with
/// `arbitrary_precision`) is `Y3006` rather than a silent `.inf`.
fn number_from_json(n: &serde_json::Number, core: &Resolver) -> Result<Value, Error> {
    if let Some(u) = n.as_u64() {
        return Ok(Value::from(u));
    }
    if let Some(i) = n.as_i64() {
        return Ok(Value::from(i));
    }
    number_from_text(&n.to_string(), core)
}

/// [`number_from_json`] for a number's JSON text.
fn number_from_text(text: &str, core: &Resolver) -> Result<Value, Error> {
    match resolve(core, &EventTag::Absent, true, text, Span::empty(0)) {
        Ok(Resolved::Number(num)) if num.is_infinite() => Err(Error::new(
            oneyaml_core::code::INVALID_VALUE,
            format!("JSON number {text} overflows a float (YAML would read it as infinity)"),
            None,
        )),
        Ok(Resolved::Number(num)) => Ok(Value::Number(num)),
        _ => {
            debug_assert!(false, "JSON number {text} is not a Core number");
            Err(Error::internal("a JSON number is not a Core number", None))
        }
    }
}

/// A JSON collection being converted.
enum JsonFrame {
    Array {
        items: std::vec::IntoIter<Json>,
        out: Sequence,
        /// Items taken so far (the path of an error).
        index: usize,
    },
    Object {
        entries: serde_json::map::IntoIter,
        out: Mapping,
        key: String,
    },
}

impl JsonFrame {
    fn finish(self) -> Value {
        match self {
            JsonFrame::Array { out, .. } => Value::Sequence(out),
            JsonFrame::Object { out, .. } => Value::Mapping(out),
        }
    }

    fn deliver(&mut self, v: Value) {
        match self {
            JsonFrame::Array { out, .. } => out.push(v),
            JsonFrame::Object { out, key, .. } => {
                out.insert(Value::String(std::mem::take(key)), v);
            }
        }
    }

    /// The next child, moved out (so it is never dropped here with its
    /// content: serde_json's `Drop` recurses).
    fn next_child(&mut self) -> Option<Json> {
        match self {
            JsonFrame::Array { items, index, .. } => items.next().inspect(|_| {
                *index = index.saturating_add(1);
            }),
            JsonFrame::Object { entries, key, .. } => entries.next().map(|(k, v)| {
                *key = k;
                v
            }),
        }
    }
}

/// A JSON node: a finished value, or a collection to fill.
enum Opened {
    Leaf(Value),
    Frame(JsonFrame),
}

fn open(j: Json, core: &Resolver) -> Result<Opened, Error> {
    Ok(match j {
        Json::Null => Opened::Leaf(Value::Null),
        Json::Bool(b) => Opened::Leaf(Value::Bool(b)),
        Json::Number(n) => Opened::Leaf(number_from_json(&n, core)?),
        Json::String(s) => Opened::Leaf(Value::String(s)),
        Json::Array(a) => Opened::Frame(JsonFrame::Array {
            out: Sequence::from(Vec::with_capacity(a.len())),
            items: a.into_iter(),
            index: 0,
        }),
        Json::Object(o) => Opened::Frame(JsonFrame::Object {
            out: Mapping::with_capacity(o.len()),
            entries: o.into_iter(),
            key: String::new(),
        }),
    })
}

/// The error with the path of the frames (outermost first), each at the
/// child it was converting.
fn located_json<'f>(e: Error, frames: impl DoubleEndedIterator<Item = &'f JsonFrame>) -> Error {
    let mut e = e;
    for frame in frames.rev() {
        e = match frame {
            JsonFrame::Array { index, .. } => e.in_index(index.saturating_sub(1)),
            JsonFrame::Object { key, .. } => e.in_key(key),
        };
    }
    e.finish(None, Vec::new())
}

/// An explicit stack (`top` innermost, `outer` around it).
/// Every JSON node is moved out of its parent and taken apart here, so no
/// JSON value with content is ever dropped by this function: serde_json's
/// recursive `Drop` runs only on leaves and emptied collections, whatever
/// the input's depth, on success and on error alike (the frames hold
/// iterators over what is left, dropped item by item). Dropping a deep
/// input the caller keeps is the caller's; this function takes it by value.
fn from_json(root: Json) -> Result<Value, Error> {
    let core = Resolver::new(&Settings::new());
    let mut top = match open(root, &core) {
        Ok(Opened::Leaf(v)) => return Ok(v),
        Ok(Opened::Frame(f)) => f,
        Err(e) => return Err(e.finish(None, Vec::new())),
    };
    let mut outer: Vec<JsonFrame> = Vec::new();
    loop {
        match top.next_child() {
            Some(child) => match open(child, &core) {
                Ok(Opened::Leaf(v)) => top.deliver(v),
                Ok(Opened::Frame(f)) => outer.push(std::mem::replace(&mut top, f)),
                Err(e) => {
                    let e = located_json(e, outer.iter().chain(std::iter::once(&top)));
                    drain(top, outer);
                    return Err(e);
                }
            },
            None => {
                let done = match outer.pop() {
                    Some(parent) => std::mem::replace(&mut top, parent).finish(),
                    None => return Ok(top.finish()),
                };
                top.deliver(done);
            }
        }
    }
}

/// Drops the rest of a failed conversion without recursion: every JSON
/// collection left is taken apart as `from_json` does.
fn drain(top: JsonFrame, outer: Vec<JsonFrame>) {
    let mut left: Vec<Json> = Vec::new();
    let mut frames = outer;
    frames.push(top);
    for mut f in frames {
        while let Some(j) = f.next_child() {
            left.push(j);
        }
    }
    while let Some(j) = left.pop() {
        match j {
            Json::Array(a) => left.extend(a),
            Json::Object(o) => left.extend(o.into_iter().map(|(_, v)| v)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn core() -> Resolver {
        Resolver::new(&Settings::new())
    }

    #[test]
    fn json_numbers_are_exact() {
        let r = core();
        let num = |n: serde_json::Number| number_from_json(&n, &r).unwrap();
        assert_eq!(num(u64::MAX.into()).as_u64(), Some(u64::MAX));
        assert_eq!(num(i64::MIN.into()).as_i64(), Some(i64::MIN));
        let n = num(serde_json::Number::from_f64(-0.0).unwrap());
        assert!(n.as_f64().unwrap().is_sign_negative());
        assert_eq!(
            num(serde_json::Number::from_f64(1e300).unwrap()).as_f64(),
            Some(1e300)
        );
    }

    /// The proof in `number_from_json`, checked: texts of every shape of
    /// JSON's number grammar (including `arbitrary_precision` ones wider
    /// than any Rust type) resolve to Core numbers.
    /// What `arbitrary_precision` can hand over (not enabled in this
    /// build, so checked on the text path it takes).
    #[test]
    fn arbitrary_precision_texts() {
        let r = core();
        let e = number_from_text("1e400", &r).unwrap_err();
        assert_eq!(e.code(), oneyaml_core::code::INVALID_VALUE);
        assert!(number_from_text("-1E+400", &r).is_err());
        assert_eq!(number_from_text("-0", &r).unwrap(), Value::from(0));
        let wide = number_from_text("123456789012345678901234567890123456789012", &r).unwrap();
        assert!(wide.as_number().unwrap().is_big_integer());
        assert_eq!(number_from_text("1e-400", &r).unwrap().as_f64(), Some(0.0));
    }

    #[test]
    fn json_number_texts_are_core_numbers() {
        let r = core();
        let ints = ["0", "7", "-0", "-12", "123456789012345678901234567890"];
        let fracs = ["", ".0", ".5", ".000123"];
        let exps = ["", "e5", "E5", "e+5", "e-5", "E-400", "e400"];
        for i in ints {
            for f in fracs {
                for e in exps {
                    let text = format!("{i}{f}{e}");
                    let got = resolve(&r, &EventTag::Absent, true, &text, Span::empty(0));
                    assert!(matches!(got, Ok(Resolved::Number(_))), "{text}");
                }
            }
        }
        // serde_json's own float texts.
        for f in [1.5, 1e300, -0.0, 5e-324, f64::MAX, f64::MIN_POSITIVE, 0.1] {
            let text = serde_json::Number::from_f64(f).unwrap().to_string();
            let got = resolve(&r, &EventTag::Absent, true, &text, Span::empty(0));
            assert!(matches!(got, Ok(Resolved::Number(_))), "{text}");
        }
    }
}
