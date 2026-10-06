//! How the tree stores a diagnostic: with its texts, or,
//! past `Limits::max_diagnostics`, as a fixed-size **skeleton** with no heap
//! allocation: code, severity, primary span, how far its other labels
//! reach before it, and a keyed fingerprint of everything it does not keep.
//!
//! A skeleton keeps the tree's record of what the parse found complete
//! (every error, every error node, every warning's place), so nothing a
//! reader or an edit decides depends on the limit; only texts are not
//! stored. Edits move a skeleton's span like any diagnostic's.
//!
//! A skeleton is 24 bytes, so a stored diagnostic
//! is 24 and an entry of the tree's list (with its element index) 32. It
//! keeps the full 64-bit fingerprint; the severity takes a byte (the
//! enum's niche), and the start (48 bits), the primary span's length (32),
//! the code (its registry index, 10), and the label reach before (15) and
//! after (15) it share 15 bytes. A skeleton whose fields do not fit there
//! (a start past 2⁴⁸ after an edit, a reach past 32 KiB, a code outside
//! the registry) is kept exactly, boxed ([`Wide`], no texts either): never
//! saturated, never a wrong record. Readers see a skeleton as a
//! [`Diagnostic`] with empty texts ([`super::SyntaxTree::is_skeleton`]);
//! the fingerprint is internal: it is keyed per process (so it
//! cannot be forged offline), never stored, exposed, or stable.

use std::borrow::Cow;
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;

use oneyaml_core::code::{INTERNAL_ERROR, REGISTRY};
use oneyaml_core::{Code, Diagnostic, Label, Severity, Span};

use super::arena::shift_span;

/// A diagnostic as the tree stores it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Stored {
    /// With its texts.
    Full(Box<Diagnostic>),
    /// Past the limit (see the module docs).
    Skel(Skel),
    /// Past the limit, with fields too wide for [`Skel`] (see the module
    /// docs).
    Wide(Box<Wide>),
}

/// A skeleton's severity (its byte gives [`Stored`] its niche).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
enum Sev {
    Error = 1,
    Warning = 2,
}

/// A skeleton: 24 bytes, no heap (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Skel {
    sev: Sev,
    /// The keyed fingerprint (all 64 bits of [`fingerprint`]).
    print: [u8; 8],
    /// `start | len << 48 | code index << 80 | back << 90 | fwd << 105`,
    /// little-endian.
    rest: [u8; 15],
}

/// A skeleton's fields, at full width.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Wide {
    start: usize,
    /// The primary span's length.
    len: usize,
    /// How far before the primary span's start the earliest of its other
    /// labels and fixes starts; `0`: none starts before.
    back: usize,
    /// How far after the primary span's end the latest of its other
    /// labels and fixes ends.
    fwd: usize,
    print: u64,
    code: Code,
    severity: Severity,
}

/// Bit widths of [`Skel::rest`]'s fields (120 bits in all).
const START_BITS: u32 = 48;
const LEN_BITS: u32 = 32;
const CODE_BITS: u32 = 10;
const REACH_BITS: u32 = 15;
/// Where they start.
const AT_LEN: u32 = 48;
const AT_CODE: u32 = 80;
const AT_BACK: u32 = 90;
const AT_FWD: u32 = 105;
const _: () = assert!(
    AT_LEN == START_BITS
        && AT_CODE == AT_LEN + LEN_BITS
        && AT_BACK == AT_CODE + CODE_BITS
        && AT_FWD == AT_BACK + REACH_BITS
        && AT_FWD + REACH_BITS == 120
);

/// `x` if it fits `bits` bits.
fn bits(x: usize, bits: u32) -> Option<u128> {
    let x = u128::try_from(x).ok()?;
    (x.wrapping_shr(bits) == 0).then_some(x)
}

/// The low `bits` bits of `x >> at`.
fn field(x: u128, at: u32, bits: u32) -> u128 {
    x.wrapping_shr(at) & 1u128.wrapping_shl(bits).wrapping_sub(1)
}

impl Skel {
    /// The packed form of `w`, if it fits (see the module docs).
    fn pack(w: &Wide) -> Option<Self> {
        let sev = match w.severity {
            Severity::Error => Sev::Error,
            Severity::Warning => Sev::Warning,
            _ => return None,
        };
        let index = REGISTRY.binary_search_by_key(&w.code, |i| i.code).ok()?;
        let rest = bits(w.start, START_BITS)?
            | bits(w.len, LEN_BITS)?.wrapping_shl(AT_LEN)
            | bits(index, CODE_BITS)?.wrapping_shl(AT_CODE)
            | bits(w.back, REACH_BITS)?.wrapping_shl(AT_BACK)
            | bits(w.fwd, REACH_BITS)?.wrapping_shl(AT_FWD);
        let mut bytes = [0u8; 15];
        for (b, s) in bytes.iter_mut().zip(rest.to_le_bytes()) {
            *b = s;
        }
        Some(Self {
            sev,
            print: w.print.to_le_bytes(),
            rest: bytes,
        })
    }

    /// The fields.
    fn unpack(self) -> Wide {
        let mut bytes = [0u8; 16];
        for (b, s) in bytes.iter_mut().zip(self.rest) {
            *b = s;
        }
        let rest = u128::from_le_bytes(bytes);
        // (Every field is at most 48 bits: it fits a `usize` wherever a
        // text of its size can exist.)
        let get = |at, n| usize::try_from(field(rest, at, n)).unwrap_or(usize::MAX);
        let code = REGISTRY.get(get(AT_CODE, CODE_BITS)).map(|i| i.code);
        debug_assert!(code.is_some(), "a skeleton's code is in the registry");
        Wide {
            start: get(0, START_BITS),
            len: get(AT_LEN, LEN_BITS),
            back: get(AT_BACK, REACH_BITS),
            fwd: get(AT_FWD, REACH_BITS),
            print: u64::from_le_bytes(self.print),
            code: code.unwrap_or(INTERNAL_ERROR),
            severity: match self.sev {
                Sev::Error => Severity::Error,
                Sev::Warning => Severity::Warning,
            },
        }
    }
}

/// The process's fingerprint key.
fn key() -> &'static std::collections::hash_map::RandomState {
    static KEY: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    KEY.get_or_init(std::collections::hash_map::RandomState::new)
}
/// The keyed fingerprint of `d`'s texts and of its labels and fixes
/// relative to its primary span (what a skeleton does not keep; offsets
/// are invariant under the shifts edits apply).
fn fingerprint(d: &Diagnostic) -> u64 {
    let mut h = key().build_hasher();
    let base = d.primary.span.start;
    let rel = |h: &mut std::collections::hash_map::DefaultHasher, s: Span| {
        h.write_u8(u8::from(s.start >= base));
        h.write_usize(s.start.abs_diff(base));
        h.write_usize(s.end.saturating_sub(s.start));
    };
    h.write(d.message.as_bytes());
    h.write_u8(0xff);
    h.write(d.primary.message.as_bytes());
    h.write_u8(0xff);
    h.write_usize(d.secondary.len());
    for l in &d.secondary {
        rel(&mut h, l.span);
        h.write(l.message.as_bytes());
        h.write_u8(0xff);
    }
    match &d.help {
        Some(t) => {
            h.write_u8(1);
            h.write(t.as_bytes());
        }
        None => h.write_u8(0),
    }
    h.write_u8(0xff);
    match &d.path {
        Some(p) => {
            h.write_u8(1);
            h.write(p.to_string().as_bytes());
        }
        None => h.write_u8(0),
    }
    h.write_usize(d.suggestions.len());
    for s in &d.suggestions {
        h.write(s.message.as_bytes());
        h.write_u8(0xff);
        h.write(format!("{:?}", s.applicability).as_bytes());
        h.write_usize(s.edits.len());
        for e in &s.edits {
            rel(&mut h, e.span);
            h.write(e.text.as_bytes());
            h.write_u8(0xff);
        }
    }
    h.finish()
}

/// The latest end among `d`'s labels and fixes.
fn last_end(d: &Diagnostic) -> usize {
    let mut at = d.primary.span.end;
    for l in &d.secondary {
        at = at.max(l.span.end);
    }
    for s in &d.suggestions {
        for e in &s.edits {
            at = at.max(e.span.end);
        }
    }
    at
}

/// The earliest start among `d`'s labels and fixes.
fn first_start(d: &Diagnostic) -> usize {
    let mut at = d.primary.span.start;
    for l in &d.secondary {
        at = at.min(l.span.start);
    }
    for s in &d.suggestions {
        for e in &s.edits {
            at = at.min(e.span.start);
        }
    }
    at
}

impl Stored {
    pub(crate) fn full(d: Diagnostic) -> Self {
        Self::Full(Box::new(d))
    }

    /// `d`'s skeleton (see the module docs).
    pub(crate) fn skeleton(d: &Diagnostic) -> Self {
        let start = d.primary.span.start;
        Self::of_fields(Wide {
            start,
            len: d.primary.span.end.saturating_sub(start),
            back: start.saturating_sub(first_start(d)),
            fwd: last_end(d).saturating_sub(d.primary.span.end),
            print: fingerprint(d),
            code: d.code,
            severity: d.severity,
        })
    }

    /// A skeleton of these fields: packed when they fit, else boxed.
    fn of_fields(w: Wide) -> Self {
        match Skel::pack(&w) {
            Some(s) => Self::Skel(s),
            None => Self::Wide(Box::new(w)),
        }
    }

    /// A skeleton's fields (`None`: with texts).
    fn fields(&self) -> Option<Cow<'_, Wide>> {
        match self {
            Self::Full(_) => None,
            Self::Skel(s) => Some(Cow::Owned(s.unpack())),
            Self::Wide(w) => Some(Cow::Borrowed(w)),
        }
    }

    pub(crate) fn code(&self) -> Code {
        match self {
            Self::Full(d) => d.code,
            Self::Skel(s) => s.unpack().code,
            Self::Wide(w) => w.code,
        }
    }

    pub(crate) fn severity(&self) -> Severity {
        match self {
            Self::Full(d) => d.severity,
            Self::Skel(s) => s.unpack().severity,
            Self::Wide(w) => w.severity,
        }
    }

    pub(crate) fn is_error(&self) -> bool {
        self.severity() == Severity::Error
    }

    #[cfg(test)]
    pub(crate) fn is_skeleton(&self) -> bool {
        !matches!(self, Self::Full(_))
    }

    /// The primary span.
    pub(crate) fn span(&self) -> Span {
        match (self, self.fields()) {
            (Self::Full(d), _) => d.primary.span,
            (_, Some(w)) => Span::new(w.start, w.start.saturating_add(w.len)),
            (_, None) => Span::empty(0),
        }
    }

    /// The earliest start among its labels and fixes.
    pub(crate) fn first_start(&self) -> usize {
        match (self, self.fields()) {
            (Self::Full(d), _) => first_start(d),
            (_, Some(w)) => w.start.saturating_sub(w.back),
            (_, None) => 0,
        }
    }

    /// The latest end among its labels and fixes.
    pub(crate) fn last_end(&self) -> usize {
        match (self, self.fields()) {
            (Self::Full(d), _) => last_end(d),
            (_, Some(w)) => self.span().end.saturating_add(w.fwd),
            (_, None) => 0,
        }
    }

    /// The fingerprint (computed for a diagnostic with texts).
    fn print(&self) -> u64 {
        match (self, self.fields()) {
            (Self::Full(d), _) => fingerprint(d),
            (_, Some(w)) => w.print,
            (_, None) => 0,
        }
    }

    /// The diagnostic readers see: itself, or a skeleton's code, severity,
    /// and primary span with empty texts (no heap allocation).
    pub(crate) fn view(&self) -> Cow<'_, Diagnostic> {
        match self {
            Self::Full(d) => Cow::Borrowed(d),
            _ => Cow::Owned(Diagnostic::new(
                self.code(),
                self.severity(),
                String::new(),
                Label::new(self.span(), String::new()),
            )),
        }
    }

    /// Moves every span by `to - from` (saturating at 0). A skeleton is
    /// re-packed: boxed if its new start no longer fits, packed again once
    /// it does (one form per record, so equal records compare equal).
    pub(crate) fn shift(&mut self, from: usize, to: usize) {
        if let Self::Full(d) = self {
            super::arena::shift_diagnostic(d, from, to);
            return;
        }
        if let Some(w) = self.fields() {
            let mut w = w.into_owned();
            w.start = shift_span(Span::empty(w.start), from, to).start;
            *self = Self::of_fields(w);
        }
    }

    /// Heap bytes held (for the tree's memory accounting).
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self, of: impl Fn(&Diagnostic) -> usize) -> usize {
        match self {
            Self::Full(d) => of(d).saturating_add(std::mem::size_of::<Diagnostic>()),
            Self::Skel(_) => 0,
            Self::Wide(_) => std::mem::size_of::<Wide>(),
        }
    }
}

/// Whether `a` and `b` are the same diagnostic: equal, or,
/// when one is a skeleton, the same code, severity, primary span, reach,
/// and fingerprint (texts, labels, and fixes): so the limit never decides
/// whether an edit is accepted.
pub(crate) fn same(a: &Stored, b: &Stored) -> bool {
    match (a, b) {
        (Stored::Full(x), Stored::Full(y)) => x == y,
        _ => {
            a.code() == b.code()
                && a.severity() == b.severity()
                && a.span() == b.span()
                && a.first_start() == b.first_start()
                && a.last_end() == b.last_end()
                && a.print() == b.print()
        }
    }
}

/// Whether skeleton `a` records diagnostic `d` (with its
/// texts): the same code, severity, primary span, reach, and fingerprint,
/// as [`same`] decides it. `false` for a record with texts.
pub(crate) fn records(a: &Stored, d: &Diagnostic) -> bool {
    matches!(a, Stored::Skel(_) | Stored::Wide(_))
        && a.code() == d.code
        && a.severity() == d.severity
        && a.span() == d.primary.span
        && a.first_start() == first_start(d)
        && a.last_end() == last_end(d)
        && a.print() == fingerprint(d)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use oneyaml_core::code::{DEPTH_LIMIT_EXCEEDED, LENIENT_INLINE_COLON};

    fn depth(current: usize) -> Diagnostic {
        Diagnostic::error(
            DEPTH_LIMIT_EXCEEDED,
            "nesting deeper than 3",
            Label::new(Span::new(10, 11), "here"),
        )
        .with_help(format!("raise Limits::max_depth (currently {current})"))
    }

    #[test]
    fn a_skeleton_is_small_and_holds_no_heap() {
        // 24 bytes with the 64-bit fingerprint, so an entry of
        // the tree's list is 32 (it was 32 and 40).
        assert_eq!(std::mem::size_of::<Skel>(), 24);
        assert_eq!(std::mem::size_of::<Stored>(), 24);
        assert_eq!(std::mem::size_of::<(u32, Stored)>(), 32);
        let s = Stored::skeleton(&depth(3));
        assert!(s.is_skeleton());
        assert_eq!(s.heap_bytes(|_| 1000), 0);
        let v = s.view();
        assert!(v.message.is_empty() && v.primary.message.is_empty());
        assert_eq!(
            (v.code, v.severity, v.primary.span),
            (DEPTH_LIMIT_EXCEEDED, Severity::Error, Span::new(10, 11))
        );
    }

    #[test]
    fn a_skeleton_matches_only_the_same_texts() {
        let full = Stored::full(depth(3));
        let skel = Stored::skeleton(&depth(3));
        assert!(same(&full, &skel) && same(&skel, &full));
        // Only a text differs: not the same.
        assert!(!same(&Stored::full(depth(2)), &skel));
        assert!(!same(&skel, &Stored::skeleton(&depth(2))));
        // Code, span, or severity differ.
        let mut other = depth(3);
        other.code = LENIENT_INLINE_COLON;
        assert!(!same(&Stored::full(other), &skel));
        let mut moved = Stored::skeleton(&depth(3));
        moved.shift(0, 1);
        assert!(!same(&full, &moved));
        let mut both = Stored::full(depth(3));
        both.shift(0, 1);
        assert!(same(&both, &moved), "shifts keep the fingerprint");
        // A label before the primary span is part of the record.
        let early = depth(3).with_secondary(Label::new(Span::new(2, 3), "start"));
        let s = Stored::skeleton(&early);
        assert_eq!(s.first_start(), 2);
        assert!(same(&Stored::full(early), &s));
        assert!(!same(&full, &s));
    }

    /// Every registered code and both severities survive the
    /// packed word, at the extremes of the start; a move keeps them.
    #[test]
    fn a_skeleton_keeps_every_code_severity_and_start() {
        for info in REGISTRY {
            for severity in [Severity::Error, Severity::Warning] {
                for start in [0usize, 1, 0xffff_fff0, usize::MAX >> 17] {
                    let mut d = depth(3);
                    d.code = info.code;
                    d.severity = severity;
                    d.primary.span = Span::new(start, start + 2);
                    let s = Stored::skeleton(&d);
                    assert!(s.is_skeleton());
                    assert_eq!(
                        (s.code(), s.severity(), s.span()),
                        (info.code, severity, d.primary.span)
                    );
                    assert!(same(&Stored::full(d.clone()), &s));
                    let mut moved = s.clone();
                    moved.shift(start, start + 5);
                    assert_eq!((moved.code(), moved.severity()), (info.code, severity));
                    assert_eq!(moved.span().start, start + 5);
                }
            }
        }
    }

    /// Fields too wide to pack are kept exactly, boxed,
    /// never saturated: a far-reaching label, a start moved past 2^48 by
    /// an edit (and back), a long span.
    #[test]
    fn wide_fields_are_kept_exactly() {
        let far = depth(3).with_secondary(Label::new(Span::new(0, 1), "far"));
        let mut d = far.clone();
        d.primary.span = Span::new(40_000, 40_001);
        let s = Stored::skeleton(&d);
        assert!(matches!(s, Stored::Wide(_)));
        assert_eq!((s.first_start(), s.span()), (0, d.primary.span));
        assert!(same(&Stored::full(d.clone()), &s));
        let mut long = depth(3);
        long.primary.span = Span::new(1, 1 + (1usize << 33));
        let s = Stored::skeleton(&long);
        assert!(matches!(s, Stored::Wide(_)));
        assert_eq!(s.span(), long.primary.span);
        // A start moved past 2^48 and back: boxed, then packed again.
        let base = Stored::skeleton(&depth(3));
        assert!(matches!(base, Stored::Skel(_)));
        let mut moved = base.clone();
        moved.shift(0, 1usize << 50);
        assert!(matches!(moved, Stored::Wide(_)));
        assert_eq!(moved.span().start, 10 + (1usize << 50));
        assert_eq!(
            (moved.code(), moved.severity()),
            (DEPTH_LIMIT_EXCEEDED, Severity::Error)
        );
        moved.shift(1usize << 50, 0);
        assert_eq!(moved, base);
        assert!(same(&moved, &Stored::full(depth(3))));
        // The fingerprint keeps all 64 bits.
        assert!(matches!(base, Stored::Skel(_)), "a skeleton");
        if let Stored::Skel(k) = base {
            assert_eq!(k.unpack().print, fingerprint(&depth(3)));
        }
    }
}
