//! The target bodies. Each panics exactly when an invariant breaks; a
//! refused input (an `Err`) is a pass as long as the invariants around it
//! hold.
//!
//! Test tooling: the reused oracles and these bodies assert, and the
//! fuzzer reports a panic as a finding.
#![allow(clippy::panic)]

use std::borrow::Cow;

use oneyaml::{
    Applicability, Code, Compat, Diagnostic, Document, DuplicateKeys, Encoding, Label, Limits,
    LineIndex, Render, Replacement, Settings, Span, Stream, Suggestion, Value,
};
use oneyaml_syntax::tree::SyntaxTree;

use crate::reuse::{
    comment_fuzz, consistency, edit_fuzz, ser_common, stream_edit_fuzz, value_differential,
};

/// The input as text: lossy UTF-8, so every byte string is a YAML source
/// (the raw-byte decoding paths are the `from_slice` target's).
pub(crate) fn text(data: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(data)
}

/// FNV-1a: the edit target derives its script seed and driver from the
/// whole input, so a replayed input replays its script.
pub(crate) fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn untrusted() -> Settings {
    Settings::new().limits(Limits::untrusted())
}

/// The reading settings of the meaning targets: `Limits::untrusted()`
/// always, plus the default limits with the other schema switches.
fn reading_settings() -> [(&'static str, Settings); 4] {
    [
        ("untrusted", untrusted()),
        (
            "merge_last_wins",
            Settings::new()
                .merge_keys(true)
                .duplicate_keys(DuplicateKeys::LastWins),
        ),
        (
            "yaml_1_1_untrusted",
            Compat::yaml_1_1().limits(Limits::untrusted()),
        ),
        (
            "lenient_untrusted",
            untrusted().lenient_inline_colons(true).merge_keys(true),
        ),
    ]
}

/// **parse**: the event stream satisfies the event contract (C1–C6), the
/// syntax tree re-emits the input byte for byte and has errors
/// exactly when the stream does, under the default and untrusted limits
/// and `lenient_inline_colons`; the decoder restores raw bytes exactly.
pub fn parse(data: &[u8]) {
    let src = text(data);
    let all = [
        Settings::new(),
        untrusted(),
        untrusted().lenient_inline_colons(true),
    ];
    for s in &all {
        let events = oneyaml_syntax::parse_with(&src, s);
        if let Err(e) = oneyaml_syntax::check_contract(&src, &events) {
            panic!("event contract: {e:?}\nsettings: {s:?}\nsource: {src:?}");
        }
        let streamed = oneyaml_syntax::events_with(&src, s).count();
        assert_eq!(streamed, events.len(), "streamed vs collected: {src:?}");
        let errors = events.iter().any(oneyaml_syntax::Event::is_error);
        let tree = SyntaxTree::parse_with(src.as_ref(), s);
        assert_eq!(
            tree.emit().as_bytes(),
            src.as_bytes(),
            "byte-identical tree emit: {src:?}"
        );
        assert_eq!(tree.has_errors(), errors, "tree errors: {src:?}");
    }
    let decoded = oneyaml_syntax::decode(data);
    assert_eq!(decoded.reencode(), data, "decoder re-encode");
    let tree = SyntaxTree::parse_with(decoded.text(), &untrusted());
    assert_eq!(tree.emit(), decoded.text(), "byte-identical decoded");
}

/// **consistency**: the three consistency checks of
/// `bench/tests/consistency.rs` on the input, under each reading setting;
/// strict-parse/serde disagreements pass only when classified as the
/// documented expansion-only kind, as past its pinned rounds.
pub fn consistency(data: &[u8]) {
    let src = text(data);
    for (name, s) in reading_settings() {
        let mut t = consistency::Tally::default();
        t.classify = true;
        consistency::check(name, &src, &s, &mut t);
    }
}

fn codes(r: Result<Value, oneyaml::Error>) -> Result<Value, Code> {
    r.map_err(|e| e.code())
}

/// **`from_slice`** over the decoder's paths (UTF-8/16/32, BOMs, invalid
/// bytes): the recovered `Document` and `Stream` give the input bytes back (a
/// byte-identical round trip), each stream document its own slice; the tree
/// reads what serde reads; valid text reads the same in every encoding, with
/// and without a BOM.
pub fn from_slice(data: &[u8]) {
    for s in [Settings::new(), untrusted()] {
        let p = Document::from_slice_recovering(data, &s);
        assert_eq!(p.value().to_bytes(), data, "(1) document bytes");
        let expected = codes(oneyaml::from_slice_with::<Value>(data, &s));
        assert_eq!(codes(p.value().to_value()), expected, "(2) tree vs serde");
        let st = Stream::from_slice_recovering(data, &s).into_value();
        assert_eq!(st.to_bytes(), data, "(1) stream bytes");
        if let Ok(doc) = Document::from_slice_with(data, &s) {
            assert_eq!(doc.to_bytes(), data, "(1) strict document bytes");
        }
    }
    consistency::check_document_bytes(data, "fuzz");
    let decoded = oneyaml_syntax::decode(data);
    if !decoded.is_valid() || data.len() > 16 * 1024 {
        return;
    }
    // The body without its BOM, re-encoded every way: the same meaning.
    let body = decoded.text();
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    let s = Settings::new();
    let want = codes(oneyaml::from_str_with::<Value>(body, &s));
    for enc in [
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Utf32Le,
        Encoding::Utf32Be,
    ] {
        let with_bom = format!("\u{feff}{body}");
        for text in [body, with_bom.as_str()] {
            let Ok(bytes) = oneyaml_syntax::encode(text, enc) else {
                continue;
            };
            // A BOM-less UTF-8 body that starts like another encoding is
            // detected as that one (§5.2); only the BOM form is exact.
            if detect_mismatch(&bytes, enc) {
                continue;
            }
            let got = codes(oneyaml::from_slice_with::<Value>(&bytes, &s));
            assert_eq!(
                got,
                want,
                "{enc:?} bom={}: {body:?}",
                text.len() != body.len()
            );
            let p = Document::from_slice_recovering(&bytes, &s);
            assert_eq!(p.value().to_bytes(), bytes, "(1) {enc:?}");
        }
    }
}

/// Whether the encoding detected for `bytes` is not `enc` (§5.2 detection
/// by the first bytes: a body starting with NUL reads as UTF-16/32).
fn detect_mismatch(bytes: &[u8], enc: Encoding) -> bool {
    oneyaml_syntax::detect_encoding(bytes) != enc
}

/// **serde**: typed reads into the representative type set of
/// `bench/tests/value_differential.rs` (scalars, collections, the four enum
/// representations, flatten, untagged, tuples, char, wide integers, the
/// k8s/GHA/compose/OpenAPI models): `from_value` ≡ `from_str` (the one
/// documented difference classified, never assumed), node reads equal the
/// whole-document read at that position, and `serde_json` conversions
/// round-trip.
pub fn serde(data: &[u8]) {
    let src = text(data);
    for (name, s) in reading_settings() {
        let mut t = value_differential::Tally::default();
        for doc in value_differential::documents(&src, &s) {
            let Ok(v) = oneyaml::from_str_with::<Value>(&doc, &s) else {
                continue;
            };
            // Triage of the serde timeouts: an input the default
            // alias budget accepts but `untrusted()` refuses as a limit
            // (Y2xxx) is an expansion bomb within budget. Each read of it
            // costs up to the budget (1M expansions, bounded by
            // design), and the forty typed reads plus one per node multiply
            // that into minutes: an oracle cost, not the library's. Its
            // `Value` read and JSON round trip are still checked.
            let bomb = oneyaml::from_str_with::<Value>(&doc, &untrusted())
                .is_err_and(|e| e.code().range() == oneyaml::CodeRange::Limits);
            // About forty whole-input typed reads, and one per checked
            // node, on every value but an expansion bomb, up to an
            // absolute node count (an oracle cost, not the library's: the
            // alias budget bounds each read; the consistency target reads
            // the rest). A cap relative to the input's
            // size skipped every moderately aliased input.
            if !bomb && nodes_at_most(&v, TYPED_CHECK_NODES) {
                value_differential::from_value_all(name, &doc, &s, &v, &mut t);
            }
            if !bomb && nodes_at_most(&v, NODE_CHECK_NODES) {
                value_differential::from_value_nodes(name, &doc, &s, &v, &mut t);
            }
            value_differential::json_round_trip(name, &v, &mut (0, 0));
        }
    }
}

/// Values with more nodes skip the per-node typed reads of [`serde`].
const NODE_CHECK_NODES: usize = 1024;

/// Values with more nodes skip every typed read of [`serde`].
const TYPED_CHECK_NODES: usize = 16_384;

/// Whether `v` has at most `max` nodes (an explicit stack).
pub(crate) fn nodes_at_most(v: &Value, max: usize) -> bool {
    let mut stack = vec![v];
    let mut n = 0usize;
    while let Some(v) = stack.pop() {
        n = n.saturating_add(1);
        if n > max {
            return false;
        }
        match v {
            Value::Sequence(q) => stack.extend(q.iter()),
            Value::Mapping(m) => {
                for (k, x) in m {
                    stack.push(k);
                    stack.push(x);
                }
            }
            Value::Tagged(t) => stack.push(&t.value),
            _ => {}
        }
    }
    true
}

/// The calls per script (the deterministic fuzzes use 10 to 12).
const STEPS: usize = 12;

/// **edit**: an edit script over the input as a document, checked by the
/// deterministic edit fuzzes' oracles (both readers agree after every accepted
/// call, refused calls change nothing, handles follow the model, meaning, the
/// comment rulebook's identity accounting, clones and snapshots): the input
/// picks one of four drivers — `edit_fuzz` (`Document`), `comment_fuzz`
/// (`Document` and `Stream`), `stream_edit_fuzz` — and the script's random
/// choices are seeded from the input's hash.
pub fn edit(data: &[u8]) {
    let h = fnv(data);
    let src = text(data).into_owned();
    // xorshift needs a non-zero state.
    let seed = h | 1;
    let parse = |s: &Settings| match Document::parse_with(src.clone(), s) {
        Ok(d) => d,
        Err(_) => Document::parse_recovering_with(src.clone(), s).into_value(),
    };
    match h & 7 {
        0..=2 => {
            let s = if h & 0x100 == 0 {
                untrusted()
            } else {
                untrusted().merge_keys(true)
            };
            let mut rng = edit_fuzz::generator::Rng(seed);
            let mut t = edit_fuzz::Tally::default();
            edit_fuzz::run_one("fuzz", parse(&s), &mut rng, STEPS, &mut t);
        }
        3 | 4 => {
            // The comment fuzz re-reads edited text with the defaults.
            let mut rng = comment_fuzz::generator::Rng(seed);
            let mut t = comment_fuzz::Tally::default();
            comment_fuzz::run("fuzz", parse(&Settings::new()), &mut rng, STEPS, &mut t);
        }
        5 => {
            let st = match Stream::parse(src.clone()) {
                Ok(st) => st,
                Err(_) => Stream::parse_recovering(src.clone()).into_value(),
            };
            let mut rng = comment_fuzz::generator::Rng(seed);
            let mut t = comment_fuzz::Tally::default();
            comment_fuzz::run_stream("fuzz", st, &mut rng, STEPS, &mut t);
        }
        _ => {
            let s = if h & 0x100 == 0 {
                untrusted()
            } else {
                untrusted().lenient_inline_colons(true).merge_keys(true)
            };
            let recovering = h & 0x200 != 0 || Stream::parse_with(src.as_str(), &s).is_err();
            let mut rng = stream_edit_fuzz::generator::Rng(seed);
            let mut t = stream_edit_fuzz::Tally::default();
            stream_edit_fuzz::run(&src, &s, recovering, &mut rng, STEPS, &mut t);
        }
    }
}

/// Reads fuzzer bytes as small numbers and strings for [`render`].
struct Cursor<'a> {
    data: &'a [u8],
}

impl Cursor<'_> {
    fn byte(&mut self) -> u8 {
        match self.data.split_first() {
            Some((b, rest)) => {
                self.data = rest;
                *b
            }
            None => 0,
        }
    }

    /// An offset near the interesting places of a `len`-byte source: in
    /// range, just past the end, or near `usize::MAX`.
    fn offset(&mut self, len: usize) -> usize {
        let b = self.byte();
        let lo = usize::from(self.byte());
        let hi = usize::from(self.byte());
        let n = lo | (hi << 8);
        match b & 7 {
            0 => usize::MAX.saturating_sub(n & 3),
            1 => len.saturating_add(n & 7),
            _ => n.checked_rem(len.saturating_add(1)).unwrap_or(0),
        }
    }

    fn span(&mut self, len: usize) -> Span {
        Span::new(self.offset(len), self.offset(len))
    }

    fn string(&mut self) -> String {
        let n = usize::from(self.byte() & 15);
        let (head, rest) = self.data.split_at(n.min(self.data.len()));
        self.data = rest;
        String::from_utf8_lossy(head).into_owned()
    }
}

/// **render**: `Render` on arbitrary diagnostics (random, out-of-range and
/// inverted spans, secondary labels, help, suggestions) against the source,
/// a different source, and an index of the wrong text; every diagnostic and
/// every error of the real pipeline renders the same with and without an
/// index. Input: the source, a NUL byte, then the diagnostic's bytes.
pub fn render(data: &[u8]) {
    let (src, rest) = match data.iter().position(|b| *b == 0) {
        Some(at) => (
            data.get(..at).unwrap_or_default(),
            data.get(at.saturating_add(1)..).unwrap_or_default(),
        ),
        None => (data, &[][..]),
    };
    let src = text(src);
    let mut c = Cursor { data: rest };
    let other = c.string();
    let code = Code::new(1u16.saturating_add(u16::from(c.byte()).saturating_mul(37)))
        .or_else(|| Code::new(1));
    let Some(code) = code else {
        return;
    };
    let len = src.len();
    let mut d = Diagnostic::warning(code, c.string(), Label::new(c.span(len), c.string()));
    for _ in 0..c.byte() & 3 {
        d = d.with_secondary(Label::new(c.span(len), c.string()));
    }
    if c.byte() & 1 == 1 {
        d = d.with_help(c.string());
    }
    for _ in 0..c.byte() & 3 {
        let edits = (0..c.byte() & 3)
            .map(|_| Replacement::new(c.span(len), c.string()))
            .collect::<Vec<_>>();
        let sug = Suggestion::new("fix", edits, Applicability::MaybeIncorrect);
        let _ = sug.apply(&src);
        d = d.with_suggestion(sug);
    }
    let index = LineIndex::new(&src);
    let out = d.render(&src);
    assert_eq!(out, d.render_indexed(&src, &index), "indexed render");
    let _ = d.render_indexed(&src, &LineIndex::new(&other));
    let _ = d.render(&other);
    let _ = d.to_string();
    for unit in [
        oneyaml::ColumnUnit::Utf8,
        oneyaml::ColumnUnit::Char,
        oneyaml::ColumnUnit::Utf16,
    ] {
        let _ = index.span(d.primary.span, unit);
        let _ = index.line_col(d.primary.span.start, unit);
    }
    // The real pipeline.
    let parsed = Document::parse_recovering_with(src.as_ref(), &untrusted());
    for diag in parsed.diagnostics() {
        assert_eq!(diag.render(&src), diag.render_indexed(&src, &index));
        let _ = diag.render(&other);
        for sug in &diag.suggestions {
            let _ = sug.apply(&src);
        }
    }
    if let Err(err) = oneyaml::from_str_with::<Value>(&src, &untrusted()) {
        assert_eq!(err.render(&src), err.render_indexed(&src, &index));
        let _ = err.render(&other);
        let _ = err.render("");
        let _ = err.to_string();
    }
}

/// **ser**: every `Value` the input reads as (each document, under the
/// default and YAML 1.1 readings, so tags, wide and radix numbers, complex
/// keys, and every string reach the writer) is written under each output
/// setting of the property gate (`ser_common::property_settings`, plus the
/// reading settings) and reads back equal (`deserialize(serialize(v))
/// == v`), deterministically, and `Document` reads the same. A
/// refusal must be a limit (`Y2xxx`) or an unwritable tag (`Y3015`).
pub fn ser(data: &[u8]) {
    let src = text(data);
    let mut values = Vec::new();
    for s in [untrusted(), Compat::yaml_1_1().limits(Limits::untrusted())] {
        for doc in oneyaml::Deserializer::from_str_with(&src, &s) {
            match <Value as ::serde::Deserialize>::deserialize(doc) {
                Ok(v) => values.push(v),
                Err(_) => break,
            }
        }
    }
    let mut writers = ser_common::property_settings();
    writers.extend(reading_settings());
    for v in &values {
        for (name, s) in &writers {
            let out = match oneyaml::to_string_with(v, s) {
                Ok(out) => out,
                Err(e) => {
                    let c = e.code();
                    assert!(
                        c.range() == oneyaml::CodeRange::Limits
                            || c == oneyaml::code::UNREPRESENTABLE_VALUE,
                        "{name}: refused with {e:?}\n{v:?}"
                    );
                    continue;
                }
            };
            let back = oneyaml::from_str_with::<Value>(&out, s)
                .unwrap_or_else(|e| panic!("{name}: output does not read ({e})\n{out}\n{v:?}"));
            assert_eq!(&back, v, "{name}: read back differs\n{out}");
            let again = oneyaml::to_string_with(&back, s)
                .unwrap_or_else(|e| panic!("{name}: second write ({e})\n{out}"));
            assert_eq!(again, out, "{name}: not deterministic");
            let doc = Document::parse_with(out.as_str(), s)
                .unwrap_or_else(|e| panic!("{name}: Document refuses output ({e})\n{out}"));
            let via_doc = Value::try_from(&doc)
                .unwrap_or_else(|e| panic!("{name}: Document meaning ({e})\n{out}"));
            assert_eq!(&via_doc, v, "{name}: Document reads the same\n{out}");
        }
    }
}
