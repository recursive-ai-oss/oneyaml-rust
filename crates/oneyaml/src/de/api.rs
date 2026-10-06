//! Public entry points: `serde_yaml`'s names, plus `*_with`
//! variants taking explicit [`Settings`].

use std::borrow::Cow;
use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;

use oneyaml_core::{Diagnostic, Settings, code};
use oneyaml_syntax::Encoding;
use serde::de::{self, DeserializeOwned, Visitor};

use super::Core;
use crate::construct::expand::Expander;
use crate::construct::feed::Feed;
use crate::construct::settings::read_diagnostics;
use crate::construct::{Ev, Node, Pull, Source, Tag};
use crate::diag_cap::Capped;
use crate::error::Error;
use crate::resolve::Resolver;

type Stream<'de> = Core<'de, Expander<'de, Feed<'de>>>;

/// A deserialization core over any semantic event source: the parser's
/// [`Feed`], or the syntax tree (`Document` → `Value`).
pub(crate) type SourceCore<'de, S> = Core<'de, Expander<'de, S>>;

fn open<'de>(src: &'de str, settings: &Settings) -> Result<(Stream<'de>, Vec<Diagnostic>), Error> {
    let feed = Feed::new(oneyaml_syntax::events_with(src, settings), src.len());
    open_source(feed, settings, settings.get_limits().get_max_diagnostics())
}

/// The most warnings a path that discards them builds:
/// an error carries these, then a `Y2010` summary counting the rest.
const DISCARDED_WARNINGS: usize = 16;

/// How many warnings a read whose warnings are discarded keeps.
fn discarding_limit(settings: &Settings) -> usize {
    DISCARDED_WARNINGS.min(settings.get_limits().get_max_diagnostics())
}

/// `limit`: how many warnings the read's list keeps:
/// `max_diagnostics`, or [`DISCARDED_WARNINGS`] on paths that discard
/// them; the collectors build at most one more, and count the rest.
fn open_source<'de, S: Source<'de>>(
    mut source: S,
    settings: &Settings,
    limit: usize,
) -> Result<(SourceCore<'de, S>, Vec<Diagnostic>), Error> {
    let setting_warnings = read_diagnostics(settings)?;
    let collector = || Capped::with_keep(limit.saturating_add(1));
    source.collect_warnings(collector());
    let expander = Expander::new(source, settings.get_limits());
    // Names joined from `%TAG` tags are charged to the same byte
    // budget as the expansion.
    let shared = expander.shared();
    let mut core = Core::new(expander, settings, shared);
    core.warnings = collector();
    Ok((core, setting_warnings))
}

/// Every warning so far, in source order (settings warnings first),
/// under the cap, with how many were found in all.
fn warnings<'de, S: Source<'de>>(
    settings: &mut Vec<Diagnostic>,
    core: &mut SourceCore<'de, S>,
    limit: usize,
) -> (Vec<Diagnostic>, usize) {
    let mut all = std::mem::take(settings);
    let (mut rest, a) = core.src.p.source_mut().take_warnings();
    let (mut more, b) = core.warnings.take();
    rest.append(&mut more);
    rest.sort_by_key(|d| d.primary.span.start);
    all.append(&mut rest);
    let extra = a.and(b);
    let found = all.len().saturating_add(extra.count);
    crate::diag_cap::cap(&mut all, extra, limit);
    (all, found)
}

/// Reads the one document of a stream (an empty stream reads as an empty
/// node, i.e. null) and checks that no second document follows.
fn single<'de, S: Source<'de>, R>(
    core: &mut SourceCore<'de, S>,
    settings: &Settings,
    f: impl FnOnce(&mut SourceCore<'de, S>) -> Result<R, Error>,
) -> Result<R, Error> {
    let ev = core.src.next()?;
    match ev.node {
        Node::StreamEnd => {
            let empty = Node::Scalar {
                value: Cow::Borrowed(""),
                plain: true,
                tag: Tag::Absent,
            };
            core.src.push_front(Ev::new(empty, ev.span));
            f(core)
        }
        Node::DocStart { yaml11 } => {
            core.resolver = Resolver::for_document(settings, yaml11);
            let value = f(core)?;
            end_document(core)?;
            let next = core.src.next()?;
            match next.node {
                Node::StreamEnd => Ok(value),
                _ => Err(Error::new(
                    code::MULTIPLE_DOCUMENTS,
                    "the input has more than one document",
                    Some(next.span),
                )
                .with_help("use oneyaml::Deserializer to read each document")),
            }
        }
        _ => Err(Error::internal("document expected", Some(ev.span))),
    }
}

fn end_document<'de, S: Source<'de>>(core: &mut SourceCore<'de, S>) -> Result<(), Error> {
    // A target that ignored an error and returned a value.
    core.check_unpoisoned()?;
    let ev = core.src.next()?;
    match ev.node {
        Node::DocEnd => Ok(()),
        _ => Err(Error::internal("document end expected", Some(ev.span))),
    }
}

/// Deserializes one YAML document from a string, with default [`Settings`]
/// (YAML 1.2, Core schema, [`Limits::default()`](crate::Limits)).
///
/// Strings borrow from `s` where the target allows (`&str` fields).
/// Warnings (e.g. `%YAML 1.1`) are **ignored**; use
/// [`from_str_with_diagnostics`] to see them. An empty input is an empty
/// (null) node; more than one document is an error
/// ([`code::MULTIPLE_DOCUMENTS`]); use [`Deserializer`] for streams.
///
/// ```
/// #[derive(Debug, serde::Deserialize)]
/// struct Port { port: u16 }
/// let p: Port = oneyaml::from_str("port: 8080").unwrap();
/// assert_eq!(p.port, 8080);
/// let e = oneyaml::from_str::<Port>("port: 99999").unwrap_err();
/// assert_eq!(e.to_string(), "integer 99999 is out of range for u16 at port (1:7)");
/// ```
///
/// # Errors
///
/// Syntax errors, schema errors, limit errors, and type mismatches, each
/// with its `Y####` code, path, and location.
pub fn from_str<'de, T: de::Deserialize<'de>>(s: &'de str) -> Result<T, Error> {
    from_str_with(s, &Settings::new())
}

/// [`from_str`] with explicit [`Settings`]. Warnings are ignored: at most
/// the first 16 are built, the rest counted; an error carries
/// those ([`Error::warnings`]), then a `Y2010` summary when there were
/// more. [`from_str_with_diagnostics`] keeps up to
/// `Limits::max_diagnostics`.
///
/// # Errors
///
/// As [`from_str`]; also an incorrect settings combination
/// ([`code::INCORRECT_SETTINGS`]).
pub fn from_str_with<'de, T: de::Deserialize<'de>>(
    s: &'de str,
    settings: &Settings,
) -> Result<T, Error> {
    from_str_discarding(s, settings)
}

/// [`from_str_with`], also returning the warnings (settings conflicts,
/// `%YAML` directives, tolerated duplicate keys, …) in source order:
/// the first `Limits::max_diagnostics`, then a
/// [`code::DIAGNOSTICS_SUPPRESSED`] summary counting the rest.
///
/// # Errors
///
/// As [`from_str_with`]; the error carries the warnings recorded before it
/// ([`Error::warnings`]).
///
/// ```
/// use oneyaml::{DuplicateKeys, Settings, Value};
///
/// let s = Settings::new().duplicate_keys(DuplicateKeys::LastWins);
/// let (v, warnings): (Value, _) =
///     oneyaml::from_str_with_diagnostics("a: 1\na: 2\n", &s).unwrap();
/// assert_eq!(v["a"].as_i64(), Some(2));
/// assert_eq!(warnings[0].code, oneyaml::code::DUPLICATE_KEY_LAST_WINS);
/// ```
pub fn from_str_with_diagnostics<'de, T: de::Deserialize<'de>>(
    s: &'de str,
    settings: &Settings,
) -> Result<(T, Vec<Diagnostic>), Error> {
    let feed = Feed::new(oneyaml_syntax::events_with(s, settings), s.len());
    from_source(feed, s, settings, None)
}

/// [`from_str_with`]'s read: warnings are discarded (see
/// [`read_discarding`]).
fn from_str_discarding<'de, T: de::Deserialize<'de>>(
    s: &'de str,
    settings: &Settings,
) -> Result<T, Error> {
    let feed = || Feed::new(oneyaml_syntax::events_with(s, settings), s.len());
    read_discarding(feed, || Cow::Borrowed(s), settings, None)
}

/// Deserializes the one document of `source` (the parser's events over
/// `text`, or the syntax tree of `text`) exactly as [`from_str`] does:
/// the same core, expander, builder, and resolver.
///
/// `lines`: an index of `text` shared across calls (a `Stream`'s
/// documents), built on the first error, so locating an error costs
/// O(log n) rather than a scan of the source.
pub(crate) fn from_source<'de, S: Source<'de>, T: de::Deserialize<'de>>(
    source: S,
    text: &'de str,
    settings: &Settings,
    lines: Option<&std::sync::OnceLock<oneyaml_core::LineIndex>>,
) -> Result<(T, Vec<Diagnostic>), Error> {
    from_source_lazy(source, || std::borrow::Cow::Borrowed(text), settings, lines)
}

/// [`from_source`] with the source text read only when an error needs it
/// (to locate and render it): a stream document's read
/// costs O(that document), never the emit of an edited stream.
pub(crate) fn from_source_lazy<'de, 't, S: Source<'de>, T: de::Deserialize<'de>>(
    source: S,
    text: impl FnOnce() -> std::borrow::Cow<'t, str>,
    settings: &Settings,
    lines: Option<&std::sync::OnceLock<oneyaml_core::LineIndex>>,
) -> Result<(T, Vec<Diagnostic>), Error> {
    let limit = settings.get_limits().get_max_diagnostics();
    read_one(source, settings, limit, true).map_err(|(e, w)| fail(e, w, text, lines))
}

/// The error of a read, located and with its warnings.
fn fail<'t>(
    e: Error,
    w: Vec<Diagnostic>,
    text: impl FnOnce() -> std::borrow::Cow<'t, str>,
    lines: Option<&std::sync::OnceLock<oneyaml_core::LineIndex>>,
) -> Error {
    let text = text();
    let e = match lines {
        Some(l) => e.locate(l.get_or_init(|| oneyaml_core::LineIndex::new(&text))),
        None => e,
    };
    e.finish(Some(&text), w)
}

/// A failed read: its error (not yet located) and its warnings.
type Failed = (Error, Vec<Diagnostic>);

/// Reads the one document of `source`, keeping `limit` warnings (see
/// [`open_source`]); `ok_warnings`: a successful read returns them
/// (otherwise they are dropped unbuilt into a list).
fn read_one<'de, S: Source<'de>, T: de::Deserialize<'de>>(
    source: S,
    settings: &Settings,
    limit: usize,
    ok_warnings: bool,
) -> Result<(T, Vec<Diagnostic>), Failed> {
    let (mut core, mut sw) = match open_source(source, settings, limit) {
        Ok(x) => x,
        Err(e) => return Err((e, Vec::new())),
    };
    match single(&mut core, settings, |c| T::deserialize(c)) {
        Ok(v) if ok_warnings => Ok((v, warnings(&mut sw, &mut core, limit).0)),
        Ok(v) => Ok((v, Vec::new())),
        Err(e) => Err((e, warnings(&mut sw, &mut core, limit).0)),
    }
}

/// A read whose warnings are discarded (`from_str`, `Document::to_value`,
/// typed node reads): at most the first
/// [`DISCARDED_WARNINGS`] are built and the rest counted, so an error
/// carries those and a `Y2010` summary when there were more; a success
/// drops them. One read only: the target's `Deserialize` runs once.
pub(crate) fn read_discarding<'de, 't, S: Source<'de>, T: de::Deserialize<'de>>(
    source: impl FnOnce() -> S,
    text: impl FnOnce() -> std::borrow::Cow<'t, str>,
    settings: &Settings,
    lines: Option<&std::sync::OnceLock<oneyaml_core::LineIndex>>,
) -> Result<T, Error> {
    read_one(source(), settings, discarding_limit(settings), false)
        .map(|(v, _)| v)
        .map_err(|(e, w)| fail(e, w, text, lines))
}

/// Deserializes one document from bytes in UTF-8, UTF-16, or UTF-32 (the
/// encoding is detected from the byte order mark or the first bytes, §5.2).
/// Error spans refer to the decoded UTF-8 text.
///
/// # Errors
///
/// As [`from_str`]; invalid encoding is a syntax error (`Y0029`–`Y0031`).
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    from_slice_with(bytes, &Settings::new())
}

/// [`from_slice`] with explicit [`Settings`].
///
/// # Errors
///
/// As [`from_slice`].
pub fn from_slice_with<T: DeserializeOwned>(bytes: &[u8], settings: &Settings) -> Result<T, Error> {
    // Only the first decoding error is reported, so
    // decoding stops there, in the one decoding pass.
    match oneyaml_syntax::decode_strict(bytes) {
        Ok(decoded) => from_str_with(decoded.text(), settings),
        Err(failed) => {
            let (d, text) = *failed;
            Err(Error::from_diagnostic(d).finish(Some(&text), Vec::new()))
        }
    }
}

/// Deserializes one document from a reader (any encoding, as
/// [`from_slice`]).
///
/// Memory is bounded by `Limits::max_document_size` (a count of decoded
/// UTF-8 bytes): the encoding is detected from the first bytes, and at most
/// that many bytes are read for UTF-8, twice as many for UTF-16, and four
/// times as many for UTF-32 (each plus a byte order mark), the most that
/// text within the limit can take in that encoding. Longer input is
/// [`code::DOCUMENT_TOO_LARGE`] without being read further.
///
/// # Errors
///
/// As [`from_slice`]; I/O failures are [`code::IO_ERROR`].
///
/// ```
/// use std::collections::BTreeMap;
///
/// // Any `Read`: a file, a socket, or bytes in memory (here UTF-16LE with a BOM).
/// let utf16: Vec<u8> = "\u{feff}port: 8080\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
/// let m: BTreeMap<String, u16> = oneyaml::from_reader(utf16.as_slice()).unwrap();
/// assert_eq!(m["port"], 8080);
/// ```
pub fn from_reader<R: Read, T: DeserializeOwned>(reader: R) -> Result<T, Error> {
    from_reader_with(reader, &Settings::new())
}

/// The most bytes that text of at most `limit` UTF-8 bytes can take in
/// `encoding`, plus its byte order mark.
fn encoded_cap(encoding: Encoding, limit: usize) -> u64 {
    let (per_byte, bom) = match encoding {
        Encoding::Utf8 => (1u64, 3u64),
        Encoding::Utf16Le | Encoding::Utf16Be => (2, 2),
        Encoding::Utf32Le | Encoding::Utf32Be => (4, 4),
        // `Encoding` is `#[non_exhaustive]`: the widest bound for an
        // encoding added later, never a rejection of valid input.
        _ => (4, 4),
    };
    u64::try_from(limit)
        .unwrap_or(u64::MAX)
        .saturating_mul(per_byte)
        .saturating_add(bom)
}

/// [`from_reader`] with explicit [`Settings`].
///
/// # Errors
///
/// As [`from_reader`].
///
/// ```
/// use oneyaml::{Limits, Settings};
///
/// let tight = Settings::new().limits(Limits::untrusted().max_document_size(16));
/// let r: Result<oneyaml::Value, _> = oneyaml::from_reader_with(&b"key: a value longer than 16 bytes\n"[..], &tight);
/// assert_eq!(r.unwrap_err().code(), oneyaml::code::DOCUMENT_TOO_LARGE);
/// ```
pub fn from_reader_with<R: Read, T: DeserializeOwned>(
    mut reader: R,
    settings: &Settings,
) -> Result<T, Error> {
    let limit = settings.get_limits().get_max_document_size();
    let io = |e: std::io::Error| Error::io(e).finish(None, Vec::new());
    // §5.2: the first four bytes decide the encoding.
    let mut buf = Vec::new();
    (&mut reader).take(4).read_to_end(&mut buf).map_err(io)?;
    let cap = encoded_cap(oneyaml_syntax::detect_encoding(&buf), limit);
    let have = u64::try_from(buf.len()).unwrap_or(u64::MAX);
    // One byte past the cap tells "too large" from "exactly at the cap".
    let rest = cap.saturating_add(1).saturating_sub(have);
    reader.take(rest).read_to_end(&mut buf).map_err(io)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > cap {
        return Err(Error::new(
            code::DOCUMENT_TOO_LARGE,
            format!("input larger than {limit} bytes"),
            None,
        )
        .with_help(format!(
            "raise Limits::max_document_size (currently {limit})"
        ))
        .finish(None, Vec::new()));
    }
    from_slice_with(&buf, settings)
}

/// A YAML stream deserializer.
///
/// Used directly as a `serde::Deserializer`, it reads exactly one document
/// (like [`from_str`]). Iterated, it yields one `Deserializer` per
/// document; documents are read in order, and a document not deserialized
/// before the next one is requested is skipped.
///
/// ```
/// let docs: Vec<i32> = oneyaml::Deserializer::from_str("1\n---\n2\n")
///     .map(|doc| serde::Deserialize::deserialize(doc))
///     .collect::<Result<_, _>>()
///     .unwrap();
/// assert_eq!(docs, [1, 2]);
/// ```
pub struct Deserializer<'de> {
    shared: Rc<RefCell<Shared<'de>>>,
    role: Role,
}

enum Role {
    Stream,
    Document(usize),
}

struct Shared<'de> {
    src: &'de str,
    settings: Settings,
    core: Option<Stream<'de>>,
    setting_warnings: Vec<Diagnostic>,
    /// Documents yielded so far.
    yielded: usize,
    /// The yielded document that may still be deserialized.
    ready: Option<usize>,
    /// Inside a document (its start read, its end not yet).
    in_doc: bool,
    /// An error to report for document `.0` (then the stream stops).
    error: Option<(usize, Error)>,
    done: bool,
}

fn unavailable() -> Error {
    Error::new(
        code::DOCUMENT_UNAVAILABLE,
        "this document is no longer available: the stream has moved past it, or it was already read",
        None,
    )
}

impl<'de> Deserializer<'de> {
    /// A deserializer over `s` with default [`Settings`].
    // `serde_yaml`'s name; `FromStr` cannot borrow from its input.
    #[allow(clippy::should_implement_trait)]
    #[must_use]
    pub fn from_str(s: &'de str) -> Self {
        Self::from_str_with(s, &Settings::new())
    }

    /// A deserializer over `s` with explicit [`Settings`].
    #[must_use]
    pub fn from_str_with(s: &'de str, settings: &Settings) -> Self {
        let (core, setting_warnings, error) = match open(s, settings) {
            Ok((c, w)) => (Some(c), w, None),
            Err(e) => (None, Vec::new(), Some((0, e))),
        };
        let shared = Shared {
            src: s,
            settings: settings.clone(),
            core,
            setting_warnings,
            yielded: 0,
            ready: None,
            in_doc: false,
            error,
            done: false,
        };
        Self {
            shared: Rc::new(RefCell::new(shared)),
            role: Role::Stream,
        }
    }

    fn run<R>(self, f: impl FnOnce(&mut Stream<'de>) -> Result<R, Error>) -> Result<R, Error> {
        let Ok(mut guard) = self.shared.try_borrow_mut() else {
            return Err(unavailable());
        };
        let sh = &mut *guard;
        let src = sh.src;
        let doc = match self.role {
            Role::Stream if sh.yielded == 0 => None,
            Role::Stream => return Err(unavailable().finish(Some(src), Vec::new())),
            Role::Document(i) => Some(i),
        };
        if let Some((at, _)) = &sh.error {
            if doc.is_none_or(|i| i == *at) {
                if let Some((_, e)) = sh.error.take() {
                    sh.done = true;
                    return Err(e.finish(Some(src), std::mem::take(&mut sh.setting_warnings)));
                }
            }
        }
        let Some(core) = sh.core.as_mut() else {
            return Err(unavailable().finish(Some(src), Vec::new()));
        };
        let result = match doc {
            None => {
                sh.yielded = 1;
                sh.done = true;
                single(core, &sh.settings, f)
            }
            Some(i) if sh.ready == Some(i) => {
                sh.ready = None;
                f(core).and_then(|r| {
                    end_document(core)?;
                    sh.in_doc = false;
                    Ok(r)
                })
            }
            Some(_) => return Err(unavailable().finish(Some(src), Vec::new())),
        };
        result.map_err(|e| {
            sh.done = true;
            let limit = core.max_diagnostics();
            e.finish(Some(src), warnings(&mut sh.setting_warnings, core, limit).0)
        })
    }

    /// Advances to the next document; `Ok(false)` at the end.
    fn advance(sh: &mut Shared<'de>) -> Result<bool, Error> {
        let Some(core) = sh.core.as_mut() else {
            return Ok(false);
        };
        if sh.in_doc {
            // Skip the rest of a document that was not read.
            sh.ready = None;
            loop {
                let ev = core.src.next()?;
                match ev.node {
                    Node::DocEnd | Node::StreamEnd => break,
                    Node::SeqEnd | Node::MapEnd => {}
                    _ if ev.starts_node() => {
                        core.src.push_front(ev);
                        core.skip_node()?;
                    }
                    _ => {}
                }
            }
            sh.in_doc = false;
        }
        let ev = core.src.next()?;
        match ev.node {
            Node::DocStart { yaml11 } => {
                core.resolver = Resolver::for_document(&sh.settings, yaml11);
                sh.in_doc = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

impl<'de> Iterator for Deserializer<'de> {
    type Item = Deserializer<'de>;

    fn next(&mut self) -> Option<Self::Item> {
        if !matches!(self.role, Role::Stream) {
            return None;
        }
        let mut guard = self.shared.try_borrow_mut().ok()?;
        let sh = &mut *guard;
        if sh.done {
            return None;
        }
        let index = sh.yielded;
        if sh.error.is_none() {
            match Self::advance(sh) {
                Ok(true) => sh.ready = Some(index),
                Ok(false) => {
                    sh.done = true;
                    return None;
                }
                Err(e) => sh.error = Some((index, e)),
            }
        } else if let Some((at, _)) = &mut sh.error {
            *at = index;
        }
        if sh.error.is_some() {
            // The handle yielded now carries the error; the
            // stream cannot resume after it, so the iterator is fused.
            sh.done = true;
        }
        sh.yielded = index.saturating_add(1);
        Some(Deserializer {
            shared: Rc::clone(&self.shared),
            role: Role::Document(index),
        })
    }
}

macro_rules! forward {
    ($($method:ident),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
            self.run(|core| core.$method(v))
        }
    )*};
}

impl<'de> de::Deserializer<'de> for Deserializer<'de> {
    type Error = Error;

    forward! {
        deserialize_any, deserialize_bool, deserialize_i8, deserialize_i16, deserialize_i32,
        deserialize_i64, deserialize_i128, deserialize_u8, deserialize_u16, deserialize_u32,
        deserialize_u64, deserialize_u128, deserialize_f32, deserialize_f64, deserialize_char,
        deserialize_str, deserialize_string, deserialize_bytes, deserialize_byte_buf,
        deserialize_option, deserialize_unit, deserialize_seq, deserialize_map,
        deserialize_identifier, deserialize_ignored_any,
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        self.run(|core| core.deserialize_unit_struct(name, v))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        self.run(|core| core.deserialize_newtype_struct(name, v))
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, v: V) -> Result<V::Value, Error> {
        self.run(|core| core.deserialize_tuple(len, v))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        v: V,
    ) -> Result<V::Value, Error> {
        self.run(|core| core.deserialize_tuple_struct(name, len, v))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        self.run(|core| core.deserialize_struct(name, fields, v))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        self.run(|core| core.deserialize_enum(name, variants, v))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn reader_cap_follows_the_encoding() {
        assert_eq!(encoded_cap(Encoding::Utf8, 10), 13);
        assert_eq!(encoded_cap(Encoding::Utf16Le, 10), 22);
        assert_eq!(encoded_cap(Encoding::Utf16Be, 10), 22);
        assert_eq!(encoded_cap(Encoding::Utf32Le, 10), 44);
        assert_eq!(encoded_cap(Encoding::Utf32Be, 10), 44);
        assert_eq!(encoded_cap(Encoding::Utf8, usize::MAX), u64::MAX);
    }
}
