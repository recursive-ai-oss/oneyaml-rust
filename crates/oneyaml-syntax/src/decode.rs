//! Input decoding (YAML 1.2.2 §5.2): bytes in UTF-8, UTF-16 LE/BE, or
//! UTF-32 LE/BE to the UTF-8 text the parser reads.
//!
//! In-house and safe: `std`'s `str::from_utf8`, `char::decode_utf16`,
//! and `char::from_u32` do the work; there are no dependencies. One forward
//! pass, O(n), no recursion.
//!
//! # Round trip
//!
//! A byte order mark is **kept** in the text as U+FEFF, where the parser
//! reads it as [`TriviaKind::ByteOrderMark`](crate::event::TriviaKind). UTF-16
//! and UTF-32 have exactly one encoding per character, so for valid input
//! `encode(decoded.text(), decoded.encoding())` reproduces the input bytes
//! exactly: the [`Encoding`] is all an emitter needs to record.
//!
//! # Invalid input
//!
//! Decoding never fails and never panics. Each maximal run of invalid bytes
//! becomes one U+FFFD in the text and one error diagnostic
//! (`INVALID_UTF8`/`INVALID_UTF16`/`INVALID_UTF32`) whose label covers the
//! U+FFFD in the text. The original bytes of each run are kept, compactly
//! ([`InvalidRuns`]: offsets, and one pool of the invalid bytes), so
//! [`Decoded::reencode`] still reproduces the input exactly. **The diagnostics are not part of the event stream**: callers
//! must check [`Decoded::diagnostics`] (a strict caller rejects the input
//! when it is not empty).

use std::borrow::Cow;

use oneyaml_core::code::{INTERNAL_ERROR, INVALID_UTF8, INVALID_UTF16, INVALID_UTF32};
use oneyaml_core::{Code, Diagnostic, Label, Span};

pub use oneyaml_core::Encoding;

/// The decoder's view of an encoding. `Encoding` is vocabulary in
/// `oneyaml-core` and `#[non_exhaustive]` there; detection yields a
/// `Unit` directly, so decoding never meets a variant it cannot read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Unit {
    Utf8,
    Utf16 { big: bool },
    Utf32 { big: bool },
}

/// The encoding a unit decodes (total).
const fn encoding_of(unit: Unit) -> Encoding {
    match unit {
        Unit::Utf8 => Encoding::Utf8,
        Unit::Utf16 { big: false } => Encoding::Utf16Le,
        Unit::Utf16 { big: true } => Encoding::Utf16Be,
        Unit::Utf32 { big: false } => Encoding::Utf32Le,
        Unit::Utf32 { big: true } => Encoding::Utf32Be,
    }
}

/// The unit of a caller's `Encoding`. Never a panic: a variant added to `-core`
/// without codec support is an internal error (`Y9000`), never a silent
/// read or write as another encoding.
fn unit(encoding: Encoding) -> Result<Unit, Box<Diagnostic>> {
    match encoding {
        Encoding::Utf8 => Ok(Unit::Utf8),
        Encoding::Utf16Le => Ok(Unit::Utf16 { big: false }),
        Encoding::Utf16Be => Ok(Unit::Utf16 { big: true }),
        Encoding::Utf32Le => Ok(Unit::Utf32 { big: false }),
        Encoding::Utf32Be => Ok(Unit::Utf32 { big: true }),
        _ => {
            debug_assert!(false, "no codec for {encoding:?}");
            Err(Box::new(Diagnostic::error(
                INTERNAL_ERROR,
                format!("no codec for the encoding {}", encoding.name()),
                Label::new(Span::empty(0), ""),
            )))
        }
    }
}

/// The unit per the table in YAML 1.2.2 §5.2: a byte order mark, or else
/// the pattern of null bytes around an ASCII first character; UTF-8
/// otherwise.
const fn detect_unit(bytes: &[u8]) -> Unit {
    match bytes {
        [0, 0, 0xFE, 0xFF, ..] | [0, 0, 0, _, ..] => Unit::Utf32 { big: true },
        [0xFF, 0xFE, 0, 0, ..] | [_, 0, 0, 0, ..] => Unit::Utf32 { big: false },
        [0xFE, 0xFF, ..] | [0, _, ..] => Unit::Utf16 { big: true },
        [0xFF, 0xFE, ..] | [_, 0, ..] => Unit::Utf16 { big: false },
        _ => Unit::Utf8,
    }
}

/// Detects the encoding of `bytes` from their first bytes (YAML 1.2.2
/// §5.2): a byte order mark, or else the pattern of null bytes around an
/// ASCII first character; UTF-8 otherwise. Detection lives here, not in
/// `oneyaml-core`, which only names encodings.
#[must_use]
pub const fn detect_encoding(bytes: &[u8]) -> Encoding {
    encoding_of(detect_unit(bytes))
}

/// An encoding this decoder can write: detected ([`Decoded::encoder`]),
/// or checked from an [`Encoding`] ([`Encoder::new`]). Encoding with it
/// never fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Encoder(Unit);

impl Default for Encoder {
    /// UTF-8.
    fn default() -> Self {
        Self(Unit::Utf8)
    }
}

impl Encoder {
    /// The encoder for `encoding`.
    ///
    /// # Errors
    ///
    /// An internal error (`Y9000`) for an encoding this version cannot
    /// write (none today: `Encoding` is `#[non_exhaustive]`).
    pub fn new(encoding: Encoding) -> Result<Self, Box<Diagnostic>> {
        unit(encoding).map(Self)
    }

    /// The encoding written.
    #[must_use]
    pub const fn encoding(self) -> Encoding {
        encoding_of(self.0)
    }

    /// Encodes `text` (byte order marks included).
    #[must_use]
    pub fn encode(self, text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        encode_into(&mut out, text, self.0);
        out
    }
}

/// The code of an encoding's decoding error.
const fn error_code(unit: Unit) -> Code {
    match unit {
        Unit::Utf8 => INVALID_UTF8,
        Unit::Utf16 { .. } => INVALID_UTF16,
        Unit::Utf32 { .. } => INVALID_UTF32,
    }
}

/// A run of invalid input bytes, replaced by one U+FFFD in the text: a
/// view into [`InvalidRuns`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InvalidRun<'r> {
    /// The U+FFFD in the decoded text (UTF-8 byte range).
    pub text: Span,
    /// The byte range of the run in the input.
    pub input: Span,
    /// The original bytes of the run.
    pub bytes: &'r [u8],
}

/// The length of U+FFFD in UTF-8: every run's text.
const REPLACEMENT_LEN: usize = char::REPLACEMENT_CHARACTER.len_utf8();

/// The invalid runs of decoded input, in input order, stored compactly:
/// per run, its text start, its input start, and where
/// its bytes start in one pool of all the invalid bytes (three `u32`s when
/// the input is small enough, else three `usize`s); no allocation per run.
/// The pool keeps every invalid byte, so the input is still reproduced
/// exactly.
///
/// Equality, hashing, and `Debug` are over the runs, never the
/// storage form.
#[derive(Clone, Default)]
pub struct InvalidRuns {
    recs: Recs,
    pool: Vec<u8>,
}

impl PartialEq for InvalidRuns {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Eq for InvalidRuns {}

impl core::hash::Hash for InvalidRuns {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(self.len());
        for run in self {
            run.hash(state);
        }
    }
}

impl core::fmt::Debug for InvalidRuns {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// `[text start, input start, pool start]` per run.
#[derive(Clone, Debug)]
enum Recs {
    Narrow(Vec<[u32; 3]>),
    Wide(Vec<[usize; 3]>),
}

impl Default for Recs {
    fn default() -> Self {
        Self::Narrow(Vec::new())
    }
}

impl Recs {
    fn len(&self) -> usize {
        match self {
            Self::Narrow(v) => v.len(),
            Self::Wide(v) => v.len(),
        }
    }

    fn get(&self, i: usize) -> Option<[usize; 3]> {
        match self {
            Self::Narrow(v) => v.get(i).map(|r| r.map(|x| x as usize)),
            Self::Wide(v) => v.get(i).copied(),
        }
    }

    /// Appends a record. Narrow records that cannot hold it become wide
    /// first (never with the records
    /// [`InvalidRuns::for_input`] chose, and never a dropped run).
    fn push(&mut self, rec: [usize; 3]) {
        if let Self::Narrow(v) = self {
            if let [Ok(a), Ok(b), Ok(c)] = rec.map(u32::try_from) {
                v.push([a, b, c]);
                return;
            }
            *self = Self::Wide(v.iter().map(|r| r.map(|x| x as usize)).collect());
        }
        if let Self::Wide(v) = self {
            v.push(rec);
        }
    }

    fn shrink_to_fit(&mut self) {
        match self {
            Self::Narrow(v) => v.shrink_to_fit(),
            Self::Wide(v) => v.shrink_to_fit(),
        }
    }

    /// The first record whose text start is at least `at`.
    fn first_at(&self, at: usize) -> usize {
        match self {
            Self::Narrow(v) => v.partition_point(|r| (r[0] as usize) < at),
            Self::Wide(v) => v.partition_point(|r| r[0] < at),
        }
    }

    fn heap_bytes(&self) -> usize {
        match self {
            Self::Narrow(v) => v.capacity().saturating_mul(size_of::<[u32; 3]>()),
            Self::Wide(v) => v.capacity().saturating_mul(size_of::<[usize; 3]>()),
        }
    }
}

impl InvalidRuns {
    /// Runs of input of `len` bytes: narrow records when every offset
    /// (the text's included: at most three bytes per input byte) fits a
    /// `u32`.
    fn for_input(len: usize) -> Self {
        let narrow = len
            .checked_mul(REPLACEMENT_LEN)
            .is_some_and(|x| u32::try_from(x).is_ok());
        let recs = if narrow {
            Recs::Narrow(Vec::new())
        } else {
            Recs::Wide(Vec::new())
        };
        Self {
            recs,
            pool: Vec::new(),
        }
    }

    /// How many runs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recs.len()
    }

    /// `true` for valid input.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Run `i`, in input order. O(1).
    #[must_use]
    pub fn get(&self, i: usize) -> Option<InvalidRun<'_>> {
        let [text, input, from] = self.recs.get(i)?;
        let to = match self.recs.get(i.checked_add(1)?) {
            Some([_, _, next]) => next,
            None => self.pool.len(),
        };
        let bytes = self.pool.get(from..to)?;
        Some(InvalidRun {
            text: Span::new(text, text.saturating_add(REPLACEMENT_LEN)),
            input: Span::new(input, input.saturating_add(bytes.len())),
            bytes,
        })
    }

    /// The first run whose text starts at or after the text offset `at`;
    /// `len()` if none does. O(log n).
    #[must_use]
    pub fn first_at(&self, at: usize) -> usize {
        self.recs.first_at(at)
    }

    /// The runs, in input order.
    #[must_use]
    pub fn iter(&self) -> InvalidRunsIter<'_> {
        InvalidRunsIter {
            runs: self,
            at: 0,
            end: self.len(),
        }
    }

    /// The heap bytes held: the records and the pool of invalid bytes
    /// (for the memory accounting).
    #[must_use]
    #[doc(hidden)]
    pub fn heap_bytes(&self) -> usize {
        self.recs.heap_bytes().saturating_add(self.pool.capacity())
    }

    /// Appends a run whose U+FFFD starts at `text`.
    fn push(&mut self, text: usize, input: usize, bytes: &[u8]) {
        self.recs.push([text, input, self.pool.len()]);
        self.pool.extend_from_slice(bytes);
    }

    /// The last run's input and text ends.
    fn last_ends(&self) -> Option<(usize, usize)> {
        let r = self.get(self.len().checked_sub(1)?)?;
        Some((r.input.end, r.text.end))
    }

    fn shrink_to_fit(&mut self) {
        self.recs.shrink_to_fit();
        self.pool.shrink_to_fit();
    }
}

impl<'r> IntoIterator for &'r InvalidRuns {
    type Item = InvalidRun<'r>;
    type IntoIter = InvalidRunsIter<'r>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The iterator of [`InvalidRuns::iter`]: O(1) per step, `nth` included.
#[derive(Clone, Debug)]
pub struct InvalidRunsIter<'r> {
    runs: &'r InvalidRuns,
    at: usize,
    end: usize,
}

impl<'r> Iterator for InvalidRunsIter<'r> {
    type Item = InvalidRun<'r>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.end {
            return None;
        }
        let r = self.runs.get(self.at);
        self.at = self.at.saturating_add(1);
        r
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.at = self.at.saturating_add(n).min(self.end);
        self.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end.saturating_sub(self.at);
        (n, Some(n))
    }
}

impl DoubleEndedIterator for InvalidRunsIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.at >= self.end {
            return None;
        }
        self.end = self.end.saturating_sub(1);
        self.runs.get(self.end)
    }
}

impl ExactSizeIterator for InvalidRunsIter<'_> {}

/// Decoded input: the text, how it was encoded, and any decoding problems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded<'a> {
    text: Cow<'a, str>,
    unit: Unit,
    invalid: InvalidRuns,
    diagnostics: Vec<Diagnostic>,
    /// Runs without a built diagnostic (past the keep budget).
    suppressed: usize,
}

impl<'a> Decoded<'a> {
    /// The decoded text, byte order marks included. Borrowed from the input
    /// for valid UTF-8.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Consumes `self`, returning the text.
    #[must_use]
    pub fn into_text(self) -> Cow<'a, str> {
        self.text
    }

    /// The detected encoding.
    #[must_use]
    pub const fn encoding(&self) -> Encoding {
        encoding_of(self.unit)
    }

    /// The encoder that writes text back in the detected encoding.
    #[must_use]
    pub const fn encoder(&self) -> Encoder {
        Encoder(self.unit)
    }

    /// Whether the input starts with a byte order mark.
    #[must_use]
    #[doc(hidden)]
    pub fn has_bom(&self) -> bool {
        self.text.starts_with('\u{feff}')
    }

    /// The invalid runs, in input order.
    #[must_use]
    pub fn invalid_runs(&self) -> &InvalidRuns {
        &self.invalid
    }

    /// One error diagnostic per invalid run, in order; empty for valid
    /// input. With [`decode_with_limit`], those of the first runs only
    /// (see [`Decoded::suppressed_diagnostics`]).
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// How many invalid runs have no built diagnostic (past the
    /// budget of [`decode_with_limit`]); [`Decoded::invalid_runs`] is
    /// always complete, and [`run_diagnostic`] builds any run's.
    #[must_use]
    pub const fn suppressed_diagnostics(&self) -> usize {
        self.suppressed
    }

    /// `true` when the input had no decoding error.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.invalid.is_empty()
    }

    /// Takes `self` apart (no copy of the runs): the built
    /// diagnostics, the invalid runs, the encoder, and the text.
    #[must_use]
    pub fn into_parts(self) -> (Vec<Diagnostic>, InvalidRuns, Encoder, Cow<'a, str>) {
        (
            self.diagnostics,
            self.invalid,
            Encoder(self.unit),
            self.text,
        )
    }

    /// Re-encodes the unedited text, restoring invalid runs from their
    /// original bytes: always equal to the input.
    #[must_use]
    pub fn reencode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.text.len());
        let mut at = 0usize;
        for run in &self.invalid {
            let before = self.text.get(at..run.text.start).unwrap_or("");
            encode_into(&mut out, before, self.unit);
            out.extend_from_slice(run.bytes);
            at = run.text.end;
        }
        encode_into(&mut out, self.text.get(at..).unwrap_or(""), self.unit);
        out
    }
}

/// Decodes `bytes`, detecting the encoding with [`detect_encoding`].
/// Never fails; see the module docs for invalid input.
#[must_use]
pub fn decode(bytes: &[u8]) -> Decoded<'_> {
    decode_with_limit(bytes, usize::MAX)
}

/// [`decode`], building the diagnostics of the first `keep` invalid runs
/// only (`Limits::max_diagnostics`); the others are
/// counted ([`Decoded::suppressed_diagnostics`]). The runs themselves are
/// always all kept, and [`run_diagnostic`] builds any run's
/// diagnostic exactly as decoding would.
#[must_use]
pub fn decode_with_limit(bytes: &[u8], keep: usize) -> Decoded<'_> {
    let unit = detect_unit(bytes);
    let mut d = Decoder {
        text: String::new(),
        unit,
        invalid: InvalidRuns::for_input(bytes.len()),
        diagnostics: Vec::new(),
        keep,
        max_runs: usize::MAX,
        full: false,
    };
    match unit {
        Unit::Utf8 => {
            if let Ok(text) = core::str::from_utf8(bytes) {
                return Decoded {
                    text: Cow::Borrowed(text),
                    unit,
                    invalid: InvalidRuns::default(),
                    diagnostics: Vec::new(),
                    suppressed: 0,
                };
            }
            d.utf8(bytes);
        }
        Unit::Utf16 { big } => d.utf16(bytes, big),
        Unit::Utf32 { big } => d.utf32(bytes, big),
    }
    let suppressed = d.invalid.len().saturating_sub(d.diagnostics.len());
    Decoded {
        text: Cow::Owned(d.text),
        unit,
        invalid: d.invalid,
        diagnostics: d.diagnostics,
        suppressed,
    }
}

/// The first decoding error of `bytes`, if any, exactly as [`decode`]
/// reports it, with the text decoded up to it: decoding stops once its
/// run is complete (a reader that fails at the first
/// error holds nothing more).
#[must_use]
pub fn first_error(bytes: &[u8]) -> Option<(Diagnostic, String)> {
    let unit = detect_unit(bytes);
    if matches!(unit, Unit::Utf8) && core::str::from_utf8(bytes).is_ok() {
        return None;
    }
    let mut d = Decoder {
        text: String::new(),
        unit,
        invalid: InvalidRuns::for_input(bytes.len()),
        diagnostics: Vec::new(),
        keep: 1,
        max_runs: 1,
        full: false,
    };
    match unit {
        Unit::Utf8 => d.utf8(bytes),
        Unit::Utf16 { big } => d.utf16(bytes, big),
        Unit::Utf32 { big } => d.utf32(bytes, big),
    }
    let text = d.text;
    d.diagnostics.into_iter().next().map(|e| (e, text))
}

/// [`decode`] for a strict reader: one pass. Valid input
/// is decoded as [`decode`] decodes it (borrowed for valid UTF-8, no
/// copy); at the first invalid run decoding stops, as [`first_error`]
/// does, and that error is returned with the text decoded up to it.
///
/// # Errors
///
/// The first decoding error, exactly as [`first_error`] reports it.
pub fn decode_strict(bytes: &[u8]) -> Result<Decoded<'_>, Box<(Diagnostic, String)>> {
    let unit = detect_unit(bytes);
    if matches!(unit, Unit::Utf8) {
        if let Ok(text) = core::str::from_utf8(bytes) {
            return Ok(Decoded {
                text: Cow::Borrowed(text),
                unit,
                invalid: InvalidRuns::default(),
                diagnostics: Vec::new(),
                suppressed: 0,
            });
        }
    }
    let mut d = Decoder {
        text: String::new(),
        unit,
        invalid: InvalidRuns::for_input(bytes.len()),
        diagnostics: Vec::new(),
        keep: 1,
        max_runs: 1,
        full: false,
    };
    match unit {
        Unit::Utf8 => d.utf8(bytes),
        Unit::Utf16 { big } => d.utf16(bytes, big),
        Unit::Utf32 { big } => d.utf32(bytes, big),
    }
    match d.diagnostics.into_iter().next() {
        Some(e) => Err(Box::new((e, d.text))),
        None => Ok(Decoded {
            text: Cow::Owned(d.text),
            unit,
            invalid: d.invalid,
            diagnostics: Vec::new(),
            suppressed: 0,
        }),
    }
}

/// The diagnostic decoding reports for `run` of input decoded as
/// `encoding` (the same as [`Decoded::diagnostics`] holds for it).
#[must_use]
pub fn run_diagnostic(encoding: Encoding, run: &InvalidRun<'_>) -> Diagnostic {
    let unit = match encoding {
        Encoding::Utf16Le => Unit::Utf16 { big: false },
        Encoding::Utf16Be => Unit::Utf16 { big: true },
        Encoding::Utf32Le => Unit::Utf32 { big: false },
        Encoding::Utf32Be => Unit::Utf32 { big: true },
        _ => Unit::Utf8,
    };
    diagnostic_of(unit, run)
}

fn diagnostic_of(unit: Unit, run: &InvalidRun<'_>) -> Diagnostic {
    let name = encoding_of(unit).name();
    let message = format!(
        "invalid {name} at input bytes {}..{}",
        run.input.start, run.input.end
    );
    Diagnostic::error(
        error_code(unit),
        message,
        Label::new(run.text, "replaced with U+FFFD"),
    )
    .with_help(format!(
        "the input was read as {name} (YAML 1.2.2 §5.2: detected from its first bytes); \
         save it as valid {name} or UTF-8"
    ))
}

/// Encodes `text` (byte order marks included) in `encoding`.
///
/// # Errors
///
/// As [`Encoder::new`]: an internal error (`Y9000`) for an encoding this
/// version cannot write, never bytes in another encoding.
pub fn encode(text: &str, encoding: Encoding) -> Result<Vec<u8>, Box<Diagnostic>> {
    Encoder::new(encoding).map(|e| e.encode(text))
}

fn encode_into(out: &mut Vec<u8>, text: &str, unit: Unit) {
    match unit {
        Unit::Utf8 => out.extend_from_slice(text.as_bytes()),
        Unit::Utf16 { big: false } => text
            .encode_utf16()
            .for_each(|u| out.extend_from_slice(&u.to_le_bytes())),
        Unit::Utf16 { big: true } => text
            .encode_utf16()
            .for_each(|u| out.extend_from_slice(&u.to_be_bytes())),
        Unit::Utf32 { big: false } => text
            .chars()
            .for_each(|c| out.extend_from_slice(&u32::from(c).to_le_bytes())),
        Unit::Utf32 { big: true } => text
            .chars()
            .for_each(|c| out.extend_from_slice(&u32::from(c).to_be_bytes())),
    }
}

struct Decoder {
    text: String,
    unit: Unit,
    invalid: InvalidRuns,
    diagnostics: Vec<Diagnostic>,
    /// How many runs get a built diagnostic.
    keep: usize,
    /// How many runs to record before decoding stops ([`first_error`]).
    max_runs: usize,
    /// A run past `max_runs` began: decoding stopped.
    full: bool,
}

impl Decoder {
    /// Records the invalid bytes `input_start..input_end`, merging them into
    /// the previous run when adjacent (into the shared pool).
    fn invalid(&mut self, input: &[u8], input_start: usize, input_end: usize) {
        let bytes = input.get(input_start..input_end).unwrap_or(&[]);
        if let Some((input_end_last, text_end_last)) = self.invalid.last_ends() {
            if input_end_last == input_start && text_end_last == self.text.len() {
                self.invalid.pool.extend_from_slice(bytes);
                return;
            }
        }
        if self.invalid.len() >= self.max_runs {
            self.full = true;
            return;
        }
        let at = self.text.len();
        self.invalid.push(at, input_start, bytes);
        self.text.push(char::REPLACEMENT_CHARACTER);
    }

    /// Turns the first `keep` runs into diagnostics (after merging;
    /// the rest are counted, never built).
    fn finish(&mut self) {
        for run in self.invalid.iter().take(self.keep) {
            self.diagnostics.push(diagnostic_of(self.unit, &run));
        }
        // The records keep exactly what they hold.
        self.invalid.shrink_to_fit();
    }

    fn utf8(&mut self, bytes: &[u8]) {
        self.text.reserve(bytes.len());
        let mut at = 0usize;
        while let Some(rest) = bytes.get(at..) {
            if rest.is_empty() {
                break;
            }
            match core::str::from_utf8(rest) {
                Ok(valid) => {
                    self.text.push_str(valid);
                    break;
                }
                Err(e) => {
                    let good = e.valid_up_to();
                    let valid = rest
                        .get(..good)
                        .and_then(|v| core::str::from_utf8(v).ok())
                        .unwrap_or("");
                    self.text.push_str(valid);
                    let bad_len = e.error_len().unwrap_or(rest.len().saturating_sub(good));
                    let start = at.saturating_add(good);
                    // A zero-length error cannot happen; guard progress anyway.
                    let end = start.saturating_add(bad_len.max(1)).min(bytes.len());
                    self.invalid(bytes, start, end);
                    if self.full {
                        break;
                    }
                    at = end;
                }
            }
        }
        self.finish();
    }

    fn utf16(&mut self, bytes: &[u8], big: bool) {
        self.text.reserve(bytes.len());
        let units = bytes.chunks_exact(2).map(|p| match p {
            [a, b] if big => u16::from_be_bytes([*a, *b]),
            [a, b] => u16::from_le_bytes([*a, *b]),
            _ => 0,
        });
        let mut at = 0usize;
        for r in char::decode_utf16(units) {
            match r {
                Ok(c) => {
                    self.text.push(c);
                    at = at.saturating_add(c.len_utf16().saturating_mul(2));
                }
                Err(_) => {
                    let end = at.saturating_add(2);
                    self.invalid(bytes, at, end);
                    if self.full {
                        break;
                    }
                    at = end;
                }
            }
        }
        if at < bytes.len() {
            self.invalid(bytes, at, bytes.len());
        }
        self.finish();
    }

    fn utf32(&mut self, bytes: &[u8], big: bool) {
        self.text.reserve(bytes.len() / 4);
        let mut at = 0usize;
        for unit in bytes.chunks_exact(4) {
            let v = match unit {
                [a, b, c, d] if big => u32::from_be_bytes([*a, *b, *c, *d]),
                [a, b, c, d] => u32::from_le_bytes([*a, *b, *c, *d]),
                _ => u32::MAX,
            };
            let end = at.saturating_add(4);
            match char::from_u32(v) {
                Some(c) => self.text.push(c),
                None => self.invalid(bytes, at, end),
            }
            if self.full {
                break;
            }
            at = end;
        }
        if at < bytes.len() {
            self.invalid(bytes, at, bytes.len());
        }
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn detects_the_spec_table() {
        // Moved from `oneyaml-core` with `Encoding::detect`.
        let cases: [(&[u8], Encoding); 10] = [
            (&[0, 0, 0xFE, 0xFF], Encoding::Utf32Be),
            (&[0, 0, 0, b'a'], Encoding::Utf32Be),
            (&[0xFF, 0xFE, 0, 0], Encoding::Utf32Le),
            (&[b'a', 0, 0, 0], Encoding::Utf32Le),
            (&[0xFE, 0xFF], Encoding::Utf16Be),
            (&[0, b'a'], Encoding::Utf16Be),
            (&[0xFF, 0xFE], Encoding::Utf16Le),
            (&[b'a', 0], Encoding::Utf16Le),
            (b"a: 1", Encoding::Utf8),
            (&[], Encoding::Utf8),
        ];
        for (bytes, enc) in cases {
            assert_eq!(detect_encoding(bytes), enc);
            assert_eq!(decode(bytes).encoding(), enc);
            assert_eq!(decode(bytes).encoder().encoding(), enc);
        }
    }

    #[test]
    fn every_encoding_has_a_codec() {
        for enc in [
            Encoding::Utf8,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Utf32Le,
            Encoding::Utf32Be,
        ] {
            let e = Encoder::new(enc).unwrap();
            assert_eq!(e.encoding(), enc);
            let bytes = encode("a: é\n", enc).unwrap();
            assert_eq!(bytes, e.encode("a: é\n"));
            let d = decode(&bytes);
            assert_eq!((d.encoding(), d.text()), (enc, "a: é\n"));
        }
        assert_eq!(Encoder::default().encoding(), Encoding::Utf8);
    }

    /// Narrow records that meet an offset past `u32` become wide
    /// (no run dropped), and equality and hashing see the runs, never the
    /// storage form.
    #[test]
    fn records_widen_and_compare_by_runs() {
        use std::hash::BuildHasher;
        let mut narrow = InvalidRuns::for_input(10);
        let mut wide = InvalidRuns::for_input(usize::MAX);
        assert!(matches!(narrow.recs, Recs::Narrow(_)));
        assert!(matches!(wide.recs, Recs::Wide(_)));
        for r in [&mut narrow, &mut wide] {
            r.push(0, 0, b"\x80");
            r.push(5, 3, b"\xff\xfe");
            r.shrink_to_fit();
        }
        assert_eq!(narrow, wide);
        let h = std::hash::RandomState::new();
        assert_eq!(h.hash_one(&narrow), h.hash_one(&wide));
        assert_eq!(format!("{narrow:?}"), format!("{wide:?}"));
        let big = usize::try_from(u64::from(u32::MAX) + 7).unwrap();
        narrow.push(big, big, b"\xc3");
        assert!(matches!(narrow.recs, Recs::Wide(_)));
        assert_eq!(narrow.len(), 3);
        let runs: Vec<_> = narrow
            .iter()
            .map(|r| (r.text.start, r.input, r.bytes))
            .collect();
        assert_eq!(
            runs,
            [
                (0, Span::new(0, 1), b"\x80".as_slice()),
                (5, Span::new(3, 5), b"\xff\xfe".as_slice()),
                (big, Span::new(big, big + 1), b"\xc3".as_slice()),
            ]
        );
        assert_ne!(narrow, wide);
    }
}
