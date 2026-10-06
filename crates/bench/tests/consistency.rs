//! The three consistency checks, over the whole
//! corpus: yaml-test-suite (valid cases, and whatever recovery makes of the
//! invalid ones), every truncation of every suite input, the benchmark
//! corpus shapes, the adversarial corpus, the suite in every decodable
//! encoding, a deterministic fuzz run of mutated suite inputs, and a
//! structured merge-key generator ([`generator`]).
//!
//! 1. `emit(tree_builder(events)) == original bytes`, at the `Document`
//!    (and `Stream`, and each stream document) level, text and encoded
//!    bytes, invalid bytes restored.
//! 2. `serde_consumer(events) == Value::from(tree_builder(events))`: the
//!    parser-fed deserializer and the tree-fed conversion of a *recovered*
//!    document agree, value or first error code.
//! 3. `Value::from(Document) == from_str::<Value>()` through the strict
//!    public entry points: both succeed with equal values, or both fail
//!    with the same first error code. A strict `Document::parse` fails
//!    exactly when serde fails, except for the pinned [`EXPANSION_ONLY`]
//!    inputs. Typed maps hold the same entries as the `Value`, and
//!    `Document::get` finds what the `Value` has, merges included.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod generator;

use generator::Rng;
use oneyaml::{Code, Compat, Document, Encoding, Limits, Settings, Stream, Value, from_str_with};

type Outcome = Result<Value, Code>;

#[derive(Default)]
pub(crate) struct Tally {
    inputs: usize,
    ok: usize,
    failed: usize,
    /// Strict `Document::parse` disagreed with serde on an error only
    /// alias expansion reveals (a pinned [`EXPANSION_ONLY`] exception).
    earlier: usize,
    /// Past the default round count ([`DEFAULT_ROUNDS`]), disagreements are
    /// not pinned but classified: only [`expansion_only`] ones pass.
    pub(crate) classify: bool,
    /// Disagreements accepted by classification.
    classified: usize,
    /// The pinned exceptions in force ([`EXPANSION_ONLY`] when `None`; a
    /// harness self-test supplies its own).
    pins: Option<&'static [Pin]>,
}

/// A pinned disagreement: (case, input, strict code or `None` for
/// success, serde code).
type Pin = (&'static str, &'static str, Option<Code>, Code);

fn serde(src: &str, s: &Settings) -> Outcome {
    from_str_with::<Value>(src, s).map_err(|e| e.code())
}

/// (1) Each document of a stream is its own slice of the input, text and
/// bytes.
fn check_documents(name: &str, src: &str, stream: &Stream) {
    for (i, d) in stream.documents().enumerate() {
        let span = d.span();
        let text = &src[span.start..span.end];
        assert_eq!(d.to_string(), text, "(1) document {i} {name}");
        assert_eq!(
            d.to_bytes(),
            text.as_bytes(),
            "(1) document bytes {i} {name}"
        );
    }
}

/// (3) `Document::get` agrees with the `Value` on string keys, at the root and
/// one level down (merge keys, duplicates, aliases: both readers agree).
fn check_navigation(name: &str, src: &str, doc: &Document, v: &Value) {
    const PROBES: &[&str] = &["a", "b", "c", "x", "k", "k0", "k1", "target", "<<", "nope"];
    let mut pairs = vec![(doc.root(), v)];
    if let (Some(root), Some(m)) = (doc.root(), v.untag_ref().as_mapping()) {
        for (k, inner) in m {
            if let Some(k) = k.as_str() {
                pairs.push((root.get(k), inner));
            }
        }
    }
    for (node, value) in pairs {
        let (Some(node), Some(map)) = (node, value.untag_ref().as_mapping()) else {
            continue;
        };
        let keys = map.keys().filter_map(Value::as_str);
        for key in PROBES.iter().copied().chain(keys) {
            let want = map.get_str(key);
            let got = node.get(key);
            assert_eq!(
                got.is_some(),
                want.is_some(),
                "(3) get {key:?} {name}: {src:?}"
            );
            if let (Some(n), Some(w)) = (got.and_then(|n| n.as_scalar()), want) {
                assert_eq!(
                    n.resolved().ok().as_ref(),
                    Some(w),
                    "(3) get {key:?} {name}"
                );
            }
        }
    }
}

/// Checks 1–3 on one input.
pub(crate) fn check(name: &str, src: &str, s: &Settings, t: &mut Tally) {
    t.inputs += 1;
    // (1) Byte-identical emit of the untouched document and stream.
    let recovered = Document::parse_recovering_with(src, s).into_value();
    assert_eq!(
        recovered.to_string().as_bytes(),
        src.as_bytes(),
        "(1) {name}"
    );
    assert_eq!(recovered.to_bytes(), src.as_bytes(), "(1) bytes {name}");
    let stream = Stream::parse_recovering_with(src, s).into_value();
    assert_eq!(
        stream.to_string().as_bytes(),
        src.as_bytes(),
        "(1) stream {name}"
    );
    check_documents(name, src, &stream);

    let expected = serde(src, s);
    // (2) The same events through the tree.
    let via_tree = recovered.to_value().map_err(|e| e.code());
    assert_eq!(via_tree, expected, "(2) {name}: {src:?}");
    // (3) The strict public entry points. A strict `Document::parse` checks
    // syntax and meaning without expanding aliases, so it fails
    // exactly when serde fails, with the same first code, except for the
    // pinned inputs where only expansion reveals serde's error.
    let strict = Document::parse_with(src, s);
    let pins = t.pins.unwrap_or(EXPANSION_ONLY);
    let pinned = |strict: Option<Code>| {
        let serde = expected.as_ref().err().copied();
        serde.is_some_and(|c| pins.contains(&(name, src, strict, c)))
    };
    // Beyond the default rounds: accepted only when of the expansion-only
    // kind, and counted.
    let classified = |t: &mut Tally, strict_at: Option<usize>| {
        let ok = t.classify && expansion_only(src, s, strict_at);
        t.classified += usize::from(ok);
        ok
    };
    match &strict {
        Ok(doc) => {
            let v = Value::try_from(doc).map_err(|e| e.code());
            assert_eq!(v, expected, "(3) {name}: {src:?}");
            let excused = pinned(None);
            t.earlier += usize::from(excused);
            let excused = excused || (expected.is_err() && classified(t, None));
            assert!(
                expected.is_ok() || excused,
                "(3) strict parse succeeded, serde failed {name}: {src:?}: {expected:?}"
            );
            if let Ok(v) = &v {
                check_navigation(name, src, doc, v);
            }
        }
        Err(e) => {
            let theirs = expected.as_ref().err().copied();
            // The same meaning error at the same path.
            let same_path = || {
                e.code().range() != oneyaml::CodeRange::Schema || {
                    let serde = from_str_with::<Value>(src, s).unwrap_err();
                    paths_agree(e.path(), serde.path())
                }
            };
            let agrees = theirs == Some(e.code()) && same_path();
            // A pinned input may fail with serde's code, but at a later error
            // (the path check tells). A pin is met only by a
            // disagreement; one whose input now agrees (same code, same
            // path) is stale, and would silently excuse a regression.
            let pin = pinned(Some(e.code()));
            assert!(
                !(pin && agrees),
                "(3) stale pin {name}: strict parsing agrees with serde now \
                 ({e:?} at {:?}); remove it from EXPANSION_ONLY",
                e.path()
            );
            let excused = pin;
            t.earlier += usize::from(excused);
            let mut at = e.diagnostic().primary.span.start;
            // An oracle fix: an invalid merge value (Y1016)
            // is reported at its `<<` key, while serde expands the value's
            // aliases first, so a budget it exhausts there comes after
            // the key: the strict error counts from the end of the mapping
            // holding the merge (its path), where its meaning is decided.
            if e.code() == oneyaml::code::INVALID_MERGE_VALUE {
                if let Some(n) = e.path().and_then(|p| recovered.get_path(p)) {
                    at = n.span().end;
                }
            }
            let excused = excused || (!agrees && classified(t, Some(at)));
            assert!(
                agrees || excused,
                "(3) {name}: {src:?}: strict {e:?} (path {:?}) vs serde {expected:?}",
                e.path()
            );
        }
    }
    check_typed_map(name, src, s, &recovered, &expected);
    check_spans(name, src, s, &recovered, strict.as_ref().err());
    match expected {
        Ok(_) => t.ok += 1,
        Err(_) => t.failed += 1,
    }
    // Streams: each document's value equals the serde stream's, when the
    // stream reads without error.
    let docs: Result<Vec<Value>, _> = oneyaml::Deserializer::from_str_with(src, s)
        .map(serde::Deserialize::deserialize)
        .collect();
    if let (Ok(docs), Ok(strict)) = (docs, Stream::parse_with(src, s)) {
        let ours: Vec<Outcome> = strict
            .documents()
            .map(|d| d.to_value().map_err(|e| e.code()))
            .collect();
        let theirs: Vec<Outcome> = docs.into_iter().map(Ok).collect();
        assert_eq!(ours, theirs, "(2) stream {name}: {src:?}");
    }
}

/// Every node's span, collected by deserializing each node as a
/// `Spanned` (aliases, merges, and tags included).
struct SpanProbe(Vec<oneyaml::Span>);

impl<'de> serde::Deserialize<'de> for SpanProbe {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let node = oneyaml::Spanned::<ProbeNode>::deserialize(d)?;
        let span = node.span();
        let mut spans = node.into_inner().0;
        spans.push(span);
        Ok(Self(spans))
    }
}

/// The spans of a node's descendants.
struct ProbeNode(Vec<oneyaml::Span>);

impl<'de> serde::Deserialize<'de> for ProbeNode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ProbeVisitor)
    }
}

struct ProbeVisitor;

impl<'de> serde::de::Visitor<'de> for ProbeVisitor {
    type Value = ProbeNode;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any node")
    }
    fn visit_bool<E>(self, _: bool) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_i64<E>(self, _: i64) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_u64<E>(self, _: u64) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_i128<E>(self, _: i128) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_u128<E>(self, _: u128) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_f64<E>(self, _: f64) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_str<E>(self, _: &str) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_unit<E>(self) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_none<E>(self) -> Result<ProbeNode, E> {
        Ok(ProbeNode(Vec::new()))
    }
    fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<ProbeNode, D::Error> {
        serde::Deserialize::deserialize(d).map(|SpanProbe(v)| ProbeNode(v))
    }
    fn visit_newtype_struct<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> Result<ProbeNode, D::Error> {
        serde::Deserialize::deserialize(d).map(|SpanProbe(v)| ProbeNode(v))
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<ProbeNode, A::Error> {
        let mut spans = Vec::new();
        while let Some(SpanProbe(v)) = a.next_element()? {
            spans.extend(v);
        }
        Ok(ProbeNode(spans))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<ProbeNode, A::Error> {
        let mut spans = Vec::new();
        while let Some((SpanProbe(k), SpanProbe(v))) = a.next_entry()? {
            spans.extend(k);
            spans.extend(v);
        }
        Ok(ProbeNode(spans))
    }
    fn visit_enum<A: serde::de::EnumAccess<'de>>(self, a: A) -> Result<ProbeNode, A::Error> {
        use serde::de::VariantAccess;
        let (_, variant): (serde::de::IgnoredAny, _) = a.variant()?;
        variant
            .newtype_variant::<SpanProbe>()
            .map(|SpanProbe(v)| ProbeNode(v))
    }
}

/// Every span a user can get is well-formed: `start <= end <= src.len()`
/// on a char boundary, so slicing the source with it never panics
/// Covers `Spanned` at every node (through aliases, merges,
/// and tags), and every label and suggestion of serde errors and
/// warnings, strict errors, and recovered diagnostics.
fn check_spans(
    name: &str,
    src: &str,
    s: &Settings,
    doc: &Document,
    strict: Option<&oneyaml::Error>,
) {
    let ok = |what: &str, sp: oneyaml::Span| {
        assert!(
            sp.start <= sp.end && src.get(sp.start..sp.end).is_some(),
            "span {}..{} ({what}) of {} bytes {name}: {src:?}",
            sp.start,
            sp.end,
            src.len()
        );
    };
    let diag = |what: &str, d: &oneyaml::Diagnostic| {
        ok(what, d.primary.span);
        for l in &d.secondary {
            ok(what, l.span);
        }
        for sg in &d.suggestions {
            for e in &sg.edits {
                ok(what, e.span);
            }
        }
    };
    let err = |what: &str, e: &oneyaml::Error| {
        diag(what, e.diagnostic());
        for w in e.warnings() {
            diag(what, w);
        }
    };
    // The probe recurses on the native stack per level: keep it
    // shallow enough for a test thread; deeper input is a `Y2007` error,
    // whose span is checked too.
    let limits = s.get_limits().clone().max_typed_depth(48);
    let probe = s.clone().limits(limits);
    match oneyaml::from_str_with_diagnostics::<SpanProbe>(src, &probe) {
        Ok((SpanProbe(spans), warnings)) => {
            for sp in spans {
                ok("Spanned", sp);
            }
            for w in &warnings {
                diag("warning", w);
            }
        }
        Err(e) => err("probe error", &e),
    }
    if let Err(e) = from_str_with::<Value>(src, s) {
        err("Value error", &e);
    }
    if let Some(e) = strict {
        err("strict error", e);
    }
    for d in doc.diagnostics() {
        diag("recovered", &d);
    }
    if let Err(e) = doc.to_value() {
        err("to_value error", &e);
    }
}

/// (3) Strict and serde paths of the same meaning error agree. A complex
/// key's segment is its shortened `Debug` text, which strict checking
/// cannot give when the key's identity needs alias expansion; such
/// a segment only has to be present.
fn paths_agree(strict: Option<&oneyaml::Path>, serde: Option<&oneyaml::Path>) -> bool {
    use oneyaml::PathSegment;
    let segs = |p: Option<&oneyaml::Path>| p.map(|p| p.segments().to_vec()).unwrap_or_default();
    let (a, b) = (segs(strict), segs(serde));
    a.len() == b.len()
        && a.iter().zip(&b).all(|(x, y)| {
            let complex = matches!(y, PathSegment::Key(k)
                if k.starts_with("Mapping {") || k.starts_with("Sequence [")
                    || k.starts_with("Tagged") || k.starts_with('*'));
            x == y || complex
        })
}

/// The kind of disagreement only alias expansion explains (the
/// classes pinned in [`EXPANSION_ONLY`]): serde's error is a duplicate key
/// or a limit, strict parsing succeeds or reports an error no earlier than
/// serde's, and the input has an alias of a collection anchor (whose
/// content strict checking never compares). The alias byte budget
/// (`Y2009`) is charged by expansion only (scalar copies, `%TAG`
/// prefixes repeated per use), whatever the anchors are: no collection
/// alias is needed for it.
fn expansion_only(src: &str, s: &Settings, strict_at: Option<usize>) -> bool {
    use oneyaml::{CodeRange, NodeKind};
    let Err(serde) = from_str_with::<Value>(src, s) else {
        return false;
    };
    let code = serde.code();
    let kind = code == oneyaml::code::DUPLICATE_KEY || code.range() == CodeRange::Limits;
    let later = strict_at.is_none_or(|at| at >= serde.diagnostic().primary.span.start);
    let stream = Stream::parse_recovering_with(src, s).into_value();
    let collection_alias = stream.documents().any(|d| {
        let mut stack: Vec<_> = d.root().into_iter().collect();
        while let Some(n) = stack.pop() {
            match n.kind() {
                NodeKind::Alias => {
                    let target = n.alias_target().map(|t| t.kind());
                    if matches!(target, Some(NodeKind::Mapping | NodeKind::Sequence)) {
                        return true;
                    }
                }
                NodeKind::Mapping => {
                    if let Some(m) = n.as_mapping() {
                        stack.extend(m.iter().flat_map(|(k, v)| [k, v]));
                    }
                }
                NodeKind::Sequence => {
                    if let Some(q) = n.as_sequence() {
                        stack.extend(q.iter());
                    }
                }
                _ => {}
            }
        }
        false
    });
    let bytes = code == oneyaml::code::ALIAS_BYTE_BUDGET_EXCEEDED;
    kind && later && (collection_alias || bytes)
}

/// (3) Typed maps stream their entries (keys checked for duplicates as
/// their values complete): the same entries as the `Value` mapping,
/// or the same first error. Only a root that is not a plain mapping may be
/// refused for its shape (a serde-range code), wherever the meaning's
/// first error is.
fn check_typed_map(name: &str, src: &str, s: &Settings, doc: &Document, expected: &Outcome) {
    use oneyaml::{CodeRange, NodeKind};
    let typed = from_str_with::<std::collections::HashMap<Value, Value>>(src, s)
        .map(|m| Value::Mapping(m.into_iter().collect()))
        .map_err(|e| e.code());
    let root_is_map = doc.root().is_some_and(|r| {
        r.resolved().kind() == NodeKind::Mapping && r.resolved().tag().unwrap().is_none()
    });
    match (&typed, expected) {
        (Ok(m), Ok(v)) => {
            let v = v.untag_ref();
            assert!(v.is_mapping(), "(3) typed {name}: {src:?}: {v:?}");
            assert_eq!(m, v, "(3) typed {name}: {src:?}");
        }
        (Err(c), Err(e)) if c == e => {}
        (Err(c), _) if c.range() == CodeRange::Serde => {
            let shape = match expected {
                Ok(v) => v.as_mapping().is_none(),
                Err(_) => !root_is_map,
            };
            assert!(
                shape,
                "(3) typed map refused {name}: {src:?}: {c:?} vs {expected:?}"
            );
        }
        (Err(c), Err(e)) => assert_eq!(c, e, "(3) typed map {name}: {src:?}"),
        (a, b) => panic!("(3) typed map {name}: {src:?}: {a:?} vs {b:?}"),
    }
}

/// Every input where strict `Document::parse` may disagree with serde:
/// (case, input, strict code or `None` for success, serde code). Only
/// errors that alias expansion alone reveals belong here; any other
/// disagreement fails the test, and every entry must be met (no stale
/// entries).
///
/// X38W ("Aliases in Flow Objects", `*a` standing for the
/// collection key `[a, b]`) and fuzz round 1216 (its mutation) were pinned
/// here; strict checking now compares an alias key with its anchor's value
/// (within a copy budget linear in the input), so they agree with serde.
///
/// Generated rounds 2140 and 9215 (a complex key that merges an
/// alias, `? {<< : *a0}`, whose identity equalled no other key) were
/// pinned here; strict checking now merges the anchor's value into the
/// key's identity (within the copy budget), so they agree with serde.
///
/// Generated round 3344 (under `Settings::new()`, merge keys
/// off: a duplicate between a collection key and an alias of an equal
/// collection anchor, X38W's class) was pinned here as meeting a later
/// duplicate first (same code, other path); now it agrees with
/// serde on code, span and path. The harness now refuses such a stale pin
/// ([`a_stale_pin_is_refused`]).
///
/// Past [`DEFAULT_ROUNDS`] (e.g. `ONEYAML_FUZZ_ROUNDS=150000`), further
/// disagreements are not pinned but must be of the [`expansion_only`]
/// kind.
const EXPANSION_ONLY: &[Pin] = &[];

fn suite() -> Vec<test_suite::Case> {
    test_suite::load_suite(&test_suite::default_data_dir()).unwrap()
}

fn settings() -> [(&'static str, Settings); 2] {
    [
        ("default", Settings::new()),
        ("yaml_1_1", Compat::yaml_1_1()),
    ]
}

fn report(what: &str, t: &Tally) {
    eprintln!(
        "{what}: {} inputs, {} with a value, {} failing identically \
         ({} pinned, {} classified, where only alias expansion finds serde's error); \
         0 disagreements",
        t.inputs, t.ok, t.failed, t.earlier, t.classified
    );
}

#[test]
fn suite_cases_and_truncations() {
    let cases = suite();
    assert!(cases.len() >= 402, "{}", cases.len());
    for (label, s) in settings() {
        let mut t = Tally::default();
        let mut truncations = Tally::default();
        for case in &cases {
            let src = String::from_utf8_lossy(&case.input);
            check(&case.id, &src, &s, &mut t);
            for (cut, _) in src.char_indices().skip(1) {
                check(&case.id, &src[..cut], &s, &mut truncations);
            }
        }
        report(&format!("suite ({label})"), &t);
        report(&format!("truncations ({label})"), &truncations);
        // No pinned exception is met in the suite (X38W, whole
        // and truncated, agrees now).
        assert_eq!(t.earlier, 0, "suite ({label})");
        assert_eq!(truncations.earlier, 0, "truncations ({label})");
    }
}

/// (1) Each stream document's bytes are its slice of the input bytes,
/// invalid bytes restored (UTF-8 input: a text offset maps to the input
/// through the decoder's invalid runs).
pub(crate) fn check_document_bytes(bytes: &[u8], what: &str) {
    let decoded = oneyaml_syntax::decode(bytes);
    if decoded.encoding() != Encoding::Utf8 {
        return;
    }
    let to_input = |at: usize| {
        let mut off = at;
        for r in decoded.invalid_runs() {
            if r.text.end <= at {
                off = off + r.input.len() - r.text.len();
            }
        }
        off
    };
    let st = Stream::from_slice_recovering(bytes, &Settings::new()).into_value();
    for (i, d) in st.documents().enumerate() {
        let span = d.span();
        let want = &bytes[to_input(span.start)..to_input(span.end)];
        assert_eq!(d.to_bytes(), want, "(1) document {i} bytes {what}");
    }
}

#[test]
fn encodings_round_trip_and_agree() {
    let mut t = Tally::default();
    for case in suite() {
        let Ok(text) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        for enc in [
            Encoding::Utf8,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Utf32Le,
            Encoding::Utf32Be,
        ] {
            let with_bom = format!("\u{feff}{text}");
            for body in [&text, &with_bom] {
                let bytes = oneyaml_syntax::encode(body, enc).unwrap();
                t.inputs += 1;
                let s = Settings::new();
                let p = Document::from_slice_recovering(&bytes, &s);
                assert_eq!(p.value().to_bytes(), bytes, "(1) {} {enc:?}", case.id);
                let expected = oneyaml::from_slice::<Value>(&bytes).map_err(|e| e.code());
                let via_tree = p.value().to_value().map_err(|e| e.code());
                assert_eq!(via_tree, expected, "(2) {} {enc:?}", case.id);
                let strict = Document::from_slice(&bytes)
                    .map_err(|e| e.code())
                    .and_then(|d| d.to_value().map_err(|e| e.code()));
                assert_eq!(strict, expected, "(3) {} {enc:?}", case.id);
                let st = Stream::from_slice_recovering(&bytes, &s).into_value();
                assert_eq!(st.to_bytes(), bytes, "(1) stream {} {enc:?}", case.id);
                check_document_bytes(&bytes, &case.id);
                if expected.is_ok() {
                    t.ok += 1;
                } else {
                    t.failed += 1;
                }
            }
        }
    }
    // Invalid bytes are restored exactly, per document too.
    for bytes in [
        &b"a: \xff\xfe\n"[..],
        b"\xef\xbb\xbf\x80",
        b"\xff\xfe\x00\xd8",
        b"a: \xff\n--- b: \xfe\n",
        b"\xfe\n--- [\xff, \xc3]\n...\n# \x80\n--- \xff\n",
    ] {
        let p = Document::from_slice_recovering(bytes, &Settings::new());
        assert_eq!(p.value().to_bytes(), bytes);
        let expected = oneyaml::from_slice::<Value>(bytes).map_err(|e| e.code());
        assert_eq!(p.value().to_value().map_err(|e| e.code()), expected);
        check_document_bytes(bytes, &format!("{bytes:?}"));
    }
    report("encodings", &t);
}

#[test]
fn benchmark_and_adversarial_corpus() {
    let mut t = Tally::default();
    for case in bench::corpus::CASES
        .iter()
        .filter(|c| c.base <= 1024 * 1024)
    {
        let src = case.generate(1);
        check(case.name, &src, &Settings::new(), &mut t);
    }
    report("bench corpus", &t);
    let mut t = Tally::default();
    let mut via_meaning = 0usize;
    for a in test_suite::adversarial::corpus() {
        let untrusted = Settings::new()
            .limits(Limits::untrusted())
            .merge_keys(a.merge_keys);
        let meaning = a.layer == test_suite::adversarial::Layer::Meaning;
        // Strict parsing never expands aliases: only these cases
        // may disagree, and only as the expansion-only kind.
        t.classify = meaning;
        check(a.name, &a.input, &untrusted, &mut t);
        via_meaning += usize::from(meaning);
        // The stream rejects it with its limit code; the alias budget
        // applies to the meaning only, never to the tree.
        match Stream::parse_with(a.input.as_str(), &untrusted) {
            Err(e) => assert_eq!(e.code(), a.code, "{}", a.name),
            Ok(stream) => {
                assert!(
                    a.code == oneyaml::code::ALIAS_BUDGET_EXCEEDED
                        || a.code == oneyaml::code::ALIAS_BYTE_BUDGET_EXCEEDED,
                    "{}",
                    a.name
                );
                let first = stream.documents().find_map(|d| d.to_value().err());
                assert_eq!(first.map(|e| e.code()), Some(a.code), "{}", a.name);
                via_meaning -= 1;
            }
        }
    }
    // Every alias-budget case reached the meaning-only branch, and
    // each disagreed with strict parsing only as expansion-only.
    assert_eq!(via_meaning, 0);
    // Three node-budget and two byte-budget cases.
    assert_eq!(t.classified, 5);
    report("adversarial corpus", &t);
}

/// Characters that steer the parser into interesting corners.
const ALPHABET: &[&str] = &[
    "-", "?", ":", ",", "[", "]", "{", "}", "#", "&a", "*a", "!", "!!int ", "|", ">", "'", "\"",
    "%", "@", "`", " ", "  ", "\n", "\t", "\r\n", "---", "...", "<<", "x", "1", "\\", "\u{feff}",
    "é", "~", "null", ".inf", "0x1F", "08",
];

fn mutate(rng: &mut Rng, src: &str) -> String {
    let mut s = src.to_owned();
    for _ in 0..=rng.below(3) {
        let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        let at = bounds[rng.below(bounds.len())];
        match rng.below(3) {
            0 => s.insert_str(at, ALPHABET[rng.below(ALPHABET.len())]),
            1 => {
                let end = bounds.iter().copied().find(|&b| b > at).unwrap_or(at);
                s.replace_range(at..end, "");
            }
            _ => {
                let end = bounds.iter().copied().find(|&b| b > at).unwrap_or(at);
                s.replace_range(at..end, ALPHABET[rng.below(ALPHABET.len())]);
            }
        }
    }
    s
}

/// Rounds of the fuzz and generator runs unless `ONEYAML_FUZZ_ROUNDS`
/// says otherwise; the pinned list is exact up to here.
const DEFAULT_ROUNDS: usize = 20_000;

fn rounds() -> usize {
    std::env::var("ONEYAML_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_ROUNDS)
}

/// The first round to run: `ONEYAML_FUZZ_FROM`, to split a deep run
/// across processes (each round's input depends only on the ones before
/// it, so skipped rounds are still generated).
fn first_round() -> usize {
    std::env::var("ONEYAML_FUZZ_FROM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

#[test]
fn fuzz_run() {
    let cases = suite();
    let inputs: Vec<String> = cases
        .iter()
        .map(|c| String::from_utf8_lossy(&c.input).into_owned())
        .collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut t = Tally::default();
    let merge = Settings::new()
        .merge_keys(true)
        .duplicate_keys(oneyaml::DuplicateKeys::LastWins);
    let from = first_round();
    for i in 0..rounds() {
        let base = &inputs[rng.below(inputs.len())];
        let src = mutate(&mut rng, base);
        if i < from {
            continue;
        }
        let s = if i % 2 == 0 {
            Settings::new()
        } else {
            merge.clone()
        };
        t.classify = i >= DEFAULT_ROUNDS;
        check(&format!("fuzz #{i}"), &src, &s, &mut t);
    }
    report("fuzz", &t);
    // No pinned round (round 1216, of X38W's class, agrees now).
    assert_eq!(t.earlier, 0, "fuzz");
}

/// The structured generator, under every merge setting.
#[test]
fn merge_generator_run() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut t = Tally::default();
    let merge = Settings::new().merge_keys(true);
    let all = [
        merge.clone(),
        merge
            .clone()
            .duplicate_keys(oneyaml::DuplicateKeys::LastWins),
        merge.schema(oneyaml::Schema::Failsafe),
        Compat::serde_yaml(),
        Settings::new(),
    ];
    let from = first_round();
    for i in 0..rounds() {
        let src = generator::generate(&mut rng);
        if i < from {
            continue;
        }
        let s = &all[i % all.len()];
        t.classify = i >= DEFAULT_ROUNDS;
        check(&format!("generated #{i}"), &src, s, &mut t);
    }
    report("merge generator", &t);
    assert!(
        t.ok > t.inputs / 4,
        "the generator mostly makes valid input"
    );
    // No pinned round (round 3344 agrees now).
    assert_eq!(t.earlier, 0, "generator");
}

/// Generated round 3344 (see [`EXPANSION_ONLY`]).
const ROUND_3344: &str = "1: x\nk1: {? {&a1 << : {? {!!str b : b} : []}, *a1 : {? {*a1 : {}} : &a1 {}, c : *a1, \
     ? {&a4 << : *a1} : *a4}, ? {c : &a4 {x : !!str 1}} : {? {? [] : &a1 b, \
     ? {!!str b : '<<'} : 2, ~ : x} : 2}} : {? [&a2 a] : &a3 a}, b : [&a4 a], k : &a0 b}\n\
     k2: x\n";

/// The harness refuses a pin whose input agrees with serde (same
/// code, same path), so no stale entry can excuse a later regression.
/// Round 3344's former pin is the witness: an error-vs-error pin with
/// matching codes, which the harness used to count as met.
#[test]
fn a_stale_pin_is_refused() {
    const STALE: &[Pin] = &[(
        "generated #3344",
        ROUND_3344,
        Some(oneyaml::code::DUPLICATE_KEY),
        oneyaml::code::DUPLICATE_KEY,
    )];
    // The round itself: no pin needed.
    let mut t = Tally::default();
    check("generated #3344", ROUND_3344, &Settings::new(), &mut t);
    assert_eq!((t.failed, t.earlier), (1, 0));
    // Pinned, it is refused as stale.
    let stale = std::panic::catch_unwind(|| {
        let mut t = Tally {
            pins: Some(STALE),
            ..Tally::default()
        };
        check("generated #3344", ROUND_3344, &Settings::new(), &mut t);
    })
    .unwrap_err();
    let msg = stale
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or_default();
    assert!(msg.contains("stale pin generated #3344"), "{msg}");
}
