//! Serialization: `to_string`, `to_writer`, and [`Serializer`] (the
//! default output layout and the settings that change it).
//!
//! `to_string`, `to_writer`, and [`Serializer`] stream: they write each
//! node as serde's calls arrive ([`stream::Streamer`]),
//! through the primitives of the one emitter ([`crate::emit`]), bounded by
//! `Limits::max_typed_depth` on serde's side and `Limits::max_depth` on
//! the output. Their bytes are exactly those of building the value's
//! [`Value`] form ([`build::ToValue`], which [`to_value`] still does) and
//! emitting it. `Value`s are never walked through serde's recursive
//! protocol: a `Value` inside the data is handed over whole and
//! written by the iterative `Value` walk.
//!
//! # Stack use
//!
//! Serializing a **recursive Rust type** (e.g. `struct Tree(Vec<Tree>)`)
//! recurses once per level, as serde's `Serialize` impls are written; each
//! level (a collection, an element or field, `Some`, a newtype) counts
//! toward `Limits::max_typed_depth` (128 by default, 64 untrusted), and one
//! level more is `Y2007`. The default fits a 2 MiB thread stack with
//! room to spare (measured for 128 levels of `struct Node {
//! name: String, children: Vec<Node> }`: about 150 KiB of stack in a
//! release build, 410 KiB in a debug build); raising the limit (or
//! `Limits::unlimited()`) makes the thread's stack size the caller's
//! responsibility. `Value`s have no such requirement at any depth.

pub(crate) mod build;
pub(crate) mod stream;

use std::fmt;
use std::io;

use serde::Serialize;
use serde::ser;

use oneyaml_core::Settings;

use crate::emit::scalar::Rules;
use crate::emit::{Start, emit_document, output_rules};
use crate::error::Error;
use crate::value::Value;
use build::ToValue;
use stream::{DocState, Frame, Streamer};

/// Converts `value` to a [`Value`] with the default settings: `serde_yaml`'s
/// `to_value`, with its signature (`value` by value; `&x` works too, as
/// `&T: Serialize`), so migrating is a `use` swap. The inverse is
/// [`from_value`](crate::from_value).
///
/// The result is bounded like every `Value` our APIs produce or consume:
/// by `Limits::max_typed_depth` while serde builds it, and by
/// `Limits::max_depth` on the finished `Value` (including `Value`s the
/// input holds, which are handed over whole).
///
/// ```
/// use std::collections::BTreeMap;
///
/// let v = oneyaml::to_value(BTreeMap::from([("port", 8080)])).unwrap();
/// assert_eq!(v["port"].as_u64(), Some(8080));
/// let back: BTreeMap<String, u16> = oneyaml::from_value(v).unwrap();
/// assert_eq!(back["port"], 8080);
/// ```
///
/// # Errors
///
/// The value's `Serialize` impl failed (`Y3012`) or drove the serializer
/// out of order (`Y3016`); it nests deeper than `Limits::max_typed_depth`
/// (`Y2007`) or the result deeper than `Limits::max_depth` (`Y2005`); or a
/// mapping has two equal keys (`Y3014`).
pub fn to_value<T: Serialize>(value: T) -> Result<Value, Error> {
    to_value_with(value, &Settings::new())
}

/// [`to_value`] with explicit settings: `Limits::max_typed_depth`
/// and `Limits::max_depth` apply.
///
/// Enums take the same `Value` form under every setting: with
/// [`Settings::resolve_tagged_variant_content`] and `Layout::SerdeYaml`,
/// [`to_string_with`] writes a newtype variant around a null, bool, or
/// number as `!Name v` while this `Value` (and so its emitted text) is the
/// map `Name: v`, because a `Value`'s tagged scalars are strings and only
/// the map form reads back as the variant.
///
/// ```
/// use oneyaml::{Limits, Settings};
///
/// let shallow = Settings::new().limits(Limits::default().max_typed_depth(1));
/// assert!(oneyaml::to_value_with([1], &shallow).is_ok());
/// let e = oneyaml::to_value_with([[1]], &shallow).unwrap_err();
/// assert_eq!(e.code(), oneyaml::code::TYPED_DEPTH_EXCEEDED);
///
/// let flat = Settings::new().max_depth(1);
/// let e = oneyaml::to_value_with(&[[1]], &flat).unwrap_err();
/// assert_eq!(e.code(), oneyaml::code::VALUE_DEPTH_EXCEEDED);
/// ```
///
/// # Errors
///
/// As [`to_value`].
pub fn to_value_with<T: Serialize>(value: T, settings: &Settings) -> Result<Value, Error> {
    let v = build_value(&value, settings)?;
    // `max_depth` applies to every `Value` our APIs hand out, as in
    // `from_value` (`de/value.rs::run`); the walk is iterative and stops
    // at the limit.
    if let Err(e) = v.check_depth(settings.get_limits()) {
        return Err(e.into_error().finish(None, Vec::new()));
    }
    Ok(v)
}

/// The value as a [`Value`] (serde side, `max_typed_depth` only); the
/// caller checks `max_depth`.
fn build_value<T: ?Sized + Serialize>(value: &T, settings: &Settings) -> Result<Value, Error> {
    build::build(
        value,
        ToValue::root(settings.get_limits().get_max_typed_depth()),
    )
    .map_err(|e| e.finish(None, Vec::new()))
}

impl Value {
    /// Writes this value as a YAML document with the default settings,
    /// straight from the reference: no clone, iterative at any depth,
    /// bounded by `Limits::max_depth`. The same output as
    /// [`to_string`], which, being generic over `Serialize`, clones a
    /// `Value` once to hand it to the emitter.
    ///
    /// ```
    /// let v: oneyaml::Value = oneyaml::from_str("a: [1, 'yes']").unwrap();
    /// assert_eq!(v.to_yaml_string().unwrap(), "a:\n  - 1\n  - 'yes'\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`to_string`]: `Y2005` past `Limits::max_depth`, `Y3015` for a
    /// value YAML cannot represent so that it reads back.
    pub fn to_yaml_string(&self) -> Result<String, Error> {
        self.to_yaml_string_with(&Settings::new())
    }

    /// [`Value::to_yaml_string`] with explicit settings.
    ///
    /// ```
    /// let v = oneyaml::Value::from(vec![oneyaml::Value::from(1), oneyaml::Value::from(2)]);
    /// let s = oneyaml::Settings::new().indent(4);
    /// assert_eq!(v.to_yaml_string_with(&s).unwrap(), "- 1\n- 2\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`to_string_with`].
    pub fn to_yaml_string_with(&self, settings: &Settings) -> Result<String, Error> {
        let rules = output_rules(settings)?;
        let mut out = String::new();
        emit_document(&mut out, self, &rules, settings.get_limits(), Start::First)
            .map_err(|e| e.finish(None, Vec::new()))?;
        Ok(out)
    }
}

/// Serializes `value` as a YAML document with the default settings:
/// every-reader-safe quoting, 2-space indent, a trailing newline.
///
/// The text is written as serde's calls arrive: memory is the output plus
/// O(depth), and the keys of the mappings being written (for duplicate
/// detection). A failed value writes nothing (the partial text is
/// dropped). A [`Value`] inside `value` is cloned once on its way to the
/// emitter (the generic `Serialize` path cannot borrow it);
/// [`Value::to_yaml_string`] writes a `&Value` without the clone.
///
/// ```
/// use std::collections::BTreeMap;
/// let mut m = BTreeMap::new();
/// m.insert("answer", vec!["yes", "42"]);
/// assert_eq!(oneyaml::to_string(&m).unwrap(), "answer:\n  - 'yes'\n  - '42'\n");
/// ```
///
/// Strings that some reader would read as something else are quoted, so the
/// output reads back as the same value under YAML 1.2 and the YAML 1.1
/// readers the test suite checks against; [`to_string_with`] takes explicit
/// [`Settings`](crate::Settings) ([`Emit`](crate::Emit): indentation, quote
/// style, layout):
///
/// ```
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Service {
///     name: String,
///     port: u16,
///     tags: Vec<String>,
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let s = Service {
///     name: "web".into(),
///     port: 8080,
///     tags: vec!["yes".into(), "1.10".into()],
/// };
/// assert_eq!(
///     oneyaml::to_string(&s)?,
///     "name: web\nport: 8080\ntags:\n  - 'yes'\n  - '1.10'\n"
/// );
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// The value's `Serialize` impl failed (`Y3012`); it nests deeper than
/// `Limits::max_typed_depth` (`Y2007`) or `Limits::max_depth` (`Y2005`); a
/// mapping has two equal keys (`Y3014`); or it holds something YAML cannot
/// represent so that it reads back (`Y3015`).
pub fn to_string<T: ?Sized + Serialize>(value: &T) -> Result<String, Error> {
    to_string_with(value, &Settings::new())
}

/// [`to_string`] with explicit settings: [`Emit`](crate::Emit) output
/// options and [`Limits`](crate::Limits).
///
/// ```
/// use std::collections::BTreeMap;
///
/// let m = BTreeMap::from([("k", vec![1])]);
/// let wide = oneyaml::Settings::new().indent(4);
/// assert_eq!(oneyaml::to_string_with(&m, &wide).unwrap(), "k:\n    - 1\n");
/// ```
///
/// # Errors
///
/// As [`to_string`], plus settings that cannot produce correct output:
/// an `Emit::indent` outside `1..=Emit::MAX_INDENT` or a non-Core schema
/// (`Y1012`), and
/// `Emit::line_width(Some(_))` (`Y1017`, wrapping is not supported).
pub fn to_string_with<T: ?Sized + Serialize>(
    value: &T,
    settings: &Settings,
) -> Result<String, Error> {
    let rules = output_rules(settings)?;
    // Streamed, no intermediate `Value`.
    let mut doc = DocState::new(settings.get_limits());
    doc.document(&rules, Start::First, value)
        .map_err(|e| e.finish(None, Vec::new()))?;
    Ok(doc.out)
}

/// Serializes `value` as a YAML document into `writer`, with the default
/// settings, and flushes it.
///
/// ```
/// let mut out = Vec::new();
/// oneyaml::to_writer(&mut out, &["a", "b"]).unwrap();
/// assert_eq!(out, b"- a\n- b\n");
/// ```
///
/// # Errors
///
/// As [`to_string`], plus writing failed (`Y3013`, [`ErrorKind::Io`](crate::ErrorKind::Io)).
pub fn to_writer<W: io::Write, T: ?Sized + Serialize>(writer: W, value: &T) -> Result<(), Error> {
    to_writer_with(writer, value, &Settings::new())
}

/// [`to_writer`] with explicit settings.
///
/// ```
/// let mut out = Vec::new();
/// let settings = oneyaml::Settings::new().max_depth(1);
/// assert!(oneyaml::to_writer_with(&mut out, &vec![vec![1]], &settings).is_err());
/// assert!(out.is_empty()); // a failed document writes nothing
/// ```
///
/// # Errors
///
/// As [`to_string_with`], plus writing failed (`Y3013`).
pub fn to_writer_with<W: io::Write, T: ?Sized + Serialize>(
    mut writer: W,
    value: &T,
    settings: &Settings,
) -> Result<(), Error> {
    let out = to_string_with(value, settings)?;
    writer.write_all(out.as_bytes()).map_err(Error::write)?;
    writer.flush().map_err(Error::write)
}

/// A serde `Serializer` writing YAML to an [`io::Write`].
///
/// Each value serialized through `&mut Serializer` is one document; the
/// second and later documents start with `---`, so one serializer
/// writes a multi-document stream:
///
/// ```
/// use serde::Serialize;
/// let mut out = Vec::new();
/// let mut ser = oneyaml::Serializer::new(&mut out);
/// 1.serialize(&mut ser).unwrap();
/// vec!["a"].serialize(&mut ser).unwrap();
/// ser.into_inner().unwrap();
/// assert_eq!(out, b"1\n---\n- a\n");
/// ```
///
/// A failed document writes nothing, so the stream stays valid.
pub struct Serializer<W> {
    writer: W,
    settings: Settings,
    rules: Option<Rules>,
    documents: usize,
    /// The document being written (its buffer is reused).
    doc: DocState,
}

/// Shows the settings and the documents written; not the writer, so `W`
/// needs no `Debug`.
impl<W> fmt::Debug for Serializer<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Serializer")
            .field("settings", &self.settings)
            .field("documents", &self.documents)
            .finish_non_exhaustive()
    }
}

impl<W: io::Write> Serializer<W> {
    /// A serializer with the default settings.
    pub fn new(writer: W) -> Self {
        Self::with_settings(writer, &Settings::new())
    }

    /// A serializer with explicit settings (checked at the first document).
    ///
    /// ```
    /// use serde::Serialize;
    ///
    /// let mut out = Vec::new();
    /// let settings = oneyaml::Settings::new().indent(4);
    /// let mut ser = oneyaml::Serializer::with_settings(&mut out, &settings);
    /// std::collections::BTreeMap::from([("k", [1])])
    ///     .serialize(&mut ser)
    ///     .unwrap();
    /// ser.into_inner().unwrap();
    /// assert_eq!(out, b"k:\n    - 1\n");
    /// ```
    pub fn with_settings(writer: W, settings: &Settings) -> Self {
        Self {
            writer,
            settings: settings.clone(),
            rules: None,
            documents: 0,
            doc: DocState::new(settings.get_limits()),
        }
    }

    /// The number of documents written so far.
    #[must_use]
    pub fn documents(&self) -> usize {
        self.documents
    }

    /// Flushes and returns the writer.
    ///
    /// # Errors
    ///
    /// Flushing failed (`Y3013`).
    pub fn into_inner(mut self) -> Result<W, Error> {
        self.writer.flush().map_err(Error::write)?;
        Ok(self.writer)
    }

    /// The output rules (checked once, at the first document) and the
    /// document state, borrowed apart.
    fn parts(&mut self) -> Result<(&mut DocState, &Rules), Error> {
        if self.rules.is_none() {
            self.rules = Some(output_rules(&self.settings)?);
        }
        match &self.rules {
            Some(rules) => Ok((&mut self.doc, rules)),
            // Set just above.
            None => Err(Error::internal("output rules missing", None)),
        }
    }

    fn start(&self) -> Start {
        if self.documents == 0 {
            Start::First
        } else {
            Start::Next
        }
    }

    /// Writes one document whose root is a single serializer call.
    fn scalar(&mut self, f: impl FnOnce(Streamer<'_>) -> Result<(), Error>) -> Result<(), Error> {
        let start = self.start();
        let (doc, rules) = self.parts()?;
        let r = f(doc.root(rules, start)).and_then(|()| doc.finish());
        self.commit(r)
    }

    /// Starts a document whose root is a collection.
    fn open(
        &mut self,
        variant: Option<&'static str>,
        seq: bool,
    ) -> Result<DocumentCompound<'_, W>, Error> {
        let start = self.start();
        let (doc, rules) = self.parts()?;
        let frame = doc.root(rules, start).root_frame(variant, seq)?;
        Ok(DocumentCompound { ser: self, frame })
    }

    /// Writes the finished document, or nothing when it failed.
    fn commit(&mut self, r: Result<(), Error>) -> Result<(), Error> {
        r.map_err(|e| e.finish(None, Vec::new()))?;
        self.writer
            .write_all(self.doc.out.as_bytes())
            .map_err(Error::write)?;
        self.documents = self.documents.saturating_add(1);
        Ok(())
    }
}

/// A collection being serialized as a document of a [`Serializer`].
pub struct DocumentCompound<'a, W> {
    ser: &'a mut Serializer<W>,
    frame: Frame,
}

/// Shows the serializer it writes to; not the document being written.
impl<W> fmt::Debug for DocumentCompound<'_, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentCompound")
            .field("ser", &self.ser)
            .finish_non_exhaustive()
    }
}

impl<W: io::Write> DocumentCompound<'_, W> {
    /// Runs `f` on the root frame with the document state, borrowed apart.
    ///
    /// An error leaves here for the value's own `Serialize` impl, which
    /// returns it as the document's: it is finished here (its path built),
    /// as [`to_string`] and [`Serializer::commit`] finish theirs,
    /// so both entry points report the same error.
    fn with(
        &mut self,
        f: impl FnOnce(&mut Frame, &mut DocState, &Rules) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let ser = &mut *self.ser;
        let r = match &ser.rules {
            Some(rules) => f(&mut self.frame, &mut ser.doc, rules),
            // A document compound exists only after `parts` set them.
            None => Err(Error::internal("output rules missing", None)),
        };
        r.map_err(|e| e.finish(None, Vec::new()))
    }

    fn end_document(self) -> Result<(), Error> {
        let ser = self.ser;
        let r = match &ser.rules {
            Some(rules) => self
                .frame
                .end(&mut ser.doc, rules)
                .and_then(|()| ser.doc.finish()),
            None => Err(Error::internal("output rules missing", None)),
        };
        ser.commit(r)
    }
}

macro_rules! scalar_documents {
    ($($method:ident($t:ty)),* $(,)?) => {$(
        fn $method(self, v: $t) -> Result<(), Error> {
            self.scalar(|s| ser::Serializer::$method(s, v))
        }
    )*};
}

impl<'a, W: io::Write> ser::Serializer for &'a mut Serializer<W> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = DocumentCompound<'a, W>;
    type SerializeTuple = DocumentCompound<'a, W>;
    type SerializeTupleStruct = DocumentCompound<'a, W>;
    type SerializeTupleVariant = DocumentCompound<'a, W>;
    type SerializeMap = DocumentCompound<'a, W>;
    type SerializeStruct = DocumentCompound<'a, W>;
    type SerializeStructVariant = DocumentCompound<'a, W>;

    scalar_documents! {
        serialize_bool(bool), serialize_i8(i8), serialize_i16(i16), serialize_i32(i32),
        serialize_i64(i64), serialize_i128(i128), serialize_u8(u8), serialize_u16(u16),
        serialize_u32(u32), serialize_u64(u64), serialize_u128(u128), serialize_f32(f32),
        serialize_f64(f64), serialize_char(char), serialize_str(&str), serialize_bytes(&[u8]),
    }

    fn serialize_none(self) -> Result<(), Error> {
        self.scalar(|s| ser::Serializer::serialize_none(s))
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), Error> {
        self.scalar(|s| ser::Serializer::serialize_some(s, v))
    }
    fn serialize_unit(self) -> Result<(), Error> {
        self.scalar(|s| ser::Serializer::serialize_unit(s))
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<(), Error> {
        self.scalar(|s| ser::Serializer::serialize_unit_struct(s, name))
    }
    fn serialize_unit_variant(
        self,
        name: &'static str,
        i: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.scalar(|s| ser::Serializer::serialize_unit_variant(s, name, i, variant))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.scalar(|s| ser::Serializer::serialize_newtype_struct(s, name, v))
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        i: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.scalar(|s| ser::Serializer::serialize_newtype_variant(s, name, i, variant, v))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<DocumentCompound<'a, W>, Error> {
        self.open(None, true)
    }
    fn serialize_tuple(self, _: usize) -> Result<DocumentCompound<'a, W>, Error> {
        self.open(None, true)
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<DocumentCompound<'a, W>, Error> {
        self.open(None, true)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<DocumentCompound<'a, W>, Error> {
        self.open(Some(variant), true)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<DocumentCompound<'a, W>, Error> {
        self.open(None, false)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<DocumentCompound<'a, W>, Error> {
        self.open(None, false)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<DocumentCompound<'a, W>, Error> {
        self.open(Some(variant), false)
    }
}

macro_rules! document_compound {
    ($trait:ident, $method:ident, element) => {
        impl<W: io::Write> ser::$trait for DocumentCompound<'_, W> {
            type Ok = ();
            type Error = Error;
            fn $method<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
                self.with(|frame, doc, rules| frame.element(doc, rules, v))
            }
            fn end(self) -> Result<(), Error> {
                self.end_document()
            }
        }
    };
    ($trait:ident, $method:ident, field) => {
        impl<W: io::Write> ser::$trait for DocumentCompound<'_, W> {
            type Ok = ();
            type Error = Error;
            fn $method<T: ?Sized + Serialize>(
                &mut self,
                name: &'static str,
                v: &T,
            ) -> Result<(), Error> {
                self.with(|frame, doc, rules| frame.field(doc, rules, name, v))
            }
            fn end(self) -> Result<(), Error> {
                self.end_document()
            }
        }
    };
}

document_compound!(SerializeSeq, serialize_element, element);
document_compound!(SerializeTuple, serialize_element, element);
document_compound!(SerializeTupleStruct, serialize_field, element);
document_compound!(SerializeTupleVariant, serialize_field, element);
document_compound!(SerializeStruct, serialize_field, field);
document_compound!(SerializeStructVariant, serialize_field, field);

impl<W: io::Write> ser::SerializeMap for DocumentCompound<'_, W> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), Error> {
        self.with(|frame, doc, _| frame.key(doc, k))
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        self.with(|frame, doc, rules| frame.value(doc, rules, v))
    }
    fn end(self) -> Result<(), Error> {
        self.end_document()
    }
}
