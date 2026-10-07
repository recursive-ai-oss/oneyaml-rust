//! The serializer differential (both readers agree), two ways:
//!
//! * **Against the recorded baseline** of the last build-then-emit
//!   serializer: [`ser_corpus::corpus`] writes every input below through
//!   the public entry points, and each group's outcomes (the text, or the
//!   error's code and path) must hash to what the same code recorded
//!   there (`crates/bench/data/ser-baseline.tsv`). This one
//!   sees any byte change, including one in the emitter both current
//!   paths share. `SOURCE` below says how it was recorded.
//! * **In process**, the streaming serializer (`to_string_with`,
//!   `to_writer_with`, the document `Serializer`) against building the
//!   value's `Value` (`to_value_with`) and emitting it
//!   (`Value::to_yaml_string_with`): same bytes, or the same error code
//!   and path. It reports the failing input directly.
//!
//! The inputs:
//!
//! 1. the benchmark models (`bench::models`, read from every real-world
//!    corpus case up to 1 MB), one document and multi-document;
//! 2. the suite-derived `Value`s, both handed over whole and re-served as
//!    plain serde data (sequences, maps with any keys, tags as newtype
//!    variants), so every node goes through the streaming path;
//! 3. a generated type set: enums in every form, `Option`, maps with
//!    non-string keys, nested empties, multi-line strings, `u128`, `char`,
//!    `f32`, handed-over `Value`s inside typed data, and errors (`Y2005`,
//!    `Y2007`, `Y3012`, `Y3014`, `Y3015`, `Y3016`), and limit boundaries;
//!
//! under the output settings (`Emit` indent, quote style, sequence indent,
//! `Layout::SerdeYaml`) and each reading setting of the compat sweep.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod ser_corpus;

use std::collections::BTreeMap;
use std::io::Write as _;

use bench::ser_run::{Data, SerWorkload, prepare};
use bench::serde_run::SerdeLib;
use oneyaml::{Emit, Layout, Limits, Settings, Value, code};
use ser_corpus::{
    Dup, DupKeys, E, FailAt, Outcome, Plain, everything, settings, suite_values, tagged,
};
use serde::{Deserialize, Serialize};

/// How the baseline was recorded:
///
/// ```sh
/// REV=f285bb1   # the last build-then-emit serializer
/// git archive $REV -- . | tar -x -C /tmp/$REV   # from this workspace
/// cp crates/bench/tests/ser_corpus/mod.rs /tmp/$REV/crates/bench/tests/ser_corpus/
/// cp crates/bench/tests/ser_differential.rs /tmp/$REV/crates/bench/tests/ser_baseline.rs
/// ONEYAML_SER_BASELINE_WRITE=$PWD/crates/bench/data/ser-baseline.tsv \
///     cargo test --release -p bench --test ser_baseline -- --exact serializer_output_matches_the_recorded_baseline
/// ```
///
/// (run in `/tmp/$REV`; only `serializer_output_matches_the_recorded_baseline`
/// runs there, the in-process tests need the current code).
///
/// Re-recorded after `resolve_tagged_variant_content` changed the output: only
/// the groups `generated/serde_yaml/compat_serde_yaml` and
/// `generated/serde_yaml-double/compat_serde_yaml`, whose output changed by
/// that setting (`Settings::resolve_tagged_variant_content` writes a newtype
/// variant around a null, bool, or number as `!Name value` under
/// `Layout::SerdeYaml`). They hold the current code's hashes; every other
/// group is still as first recorded, byte for byte.
const SOURCE: &str = "crates/bench/data/ser-baseline.tsv";

/// FNV-1a 64.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
}

/// Every group of [`ser_corpus::corpus`] as `group \t items \t hash` lines,
/// in order: the hash covers each item's label and outcome.
fn baseline_lines() -> Vec<String> {
    let mut groups: Vec<(String, usize, u64)> = Vec::new();
    ser_corpus::corpus(&mut |group, item, outcome| {
        if groups.last().is_none_or(|(g, _, _)| g != group) {
            groups.push((group.to_owned(), 0, 0xcbf2_9ce4_8422_2325));
        }
        let (_, n, h) = groups.last_mut().unwrap();
        *n += 1;
        fnv(h, item.as_bytes());
        match outcome {
            Outcome::Ok(text) => {
                fnv(h, b"\0ok\0");
                fnv(h, text.as_bytes());
            }
            Outcome::Err { code, path } => {
                fnv(h, b"\0err\0");
                fnv(h, code.as_bytes());
                fnv(h, b"\0");
                fnv(h, path.as_deref().unwrap_or("-").as_bytes());
            }
        }
        fnv(h, b"\x1e");
    });
    groups
        .into_iter()
        .map(|(g, n, h)| format!("{g}\t{n}\t{h:016x}"))
        .collect()
}

#[test]
fn serializer_output_matches_the_recorded_baseline() {
    let lines = baseline_lines();
    if let Ok(path) = std::env::var("ONEYAML_SER_BASELINE_WRITE") {
        let mut text = String::from(
            "# group\titems\tfnv1a64 of labels and outcomes (see ser_differential.rs)\n",
        );
        for l in &lines {
            text.push_str(l);
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let recorded = std::fs::read_to_string(dir.join("data/ser-baseline.tsv"))
        .unwrap_or_else(|e| panic!("{SOURCE}: {e}"));
    let recorded: Vec<&str> = recorded.lines().filter(|l| !l.starts_with('#')).collect();
    let differing: Vec<String> = lines
        .iter()
        .zip(recorded.iter())
        .filter(|(now, then)| now.as_str() != **then)
        .map(|(now, then)| format!("now  {now}\nthen {then}"))
        .collect();
    assert!(
        differing.is_empty() && lines.len() == recorded.len(),
        "{} of {} groups differ from {SOURCE} ({} vs {} groups):\n{}",
        differing.len(),
        lines.len(),
        lines.len(),
        recorded.len(),
        differing.join("\n")
    );
    let items: usize = lines
        .iter()
        .map(|l| l.split('\t').nth(1).unwrap().parse::<usize>().unwrap())
        .sum();
    assert!(items > 50_000, "{items} items");
    println!(
        "{} groups, {items} items equal to the baseline",
        lines.len()
    );
}

/// The path the streaming serializer replaced.
fn old<T: ?Sized + Serialize>(v: &T, s: &Settings) -> Outcome {
    Outcome::of(oneyaml::to_value_with(v, s).and_then(|v| v.to_yaml_string_with(s)))
}

/// The streaming path, through `to_string_with` and `to_writer_with`
/// (which must agree, and write nothing on failure).
fn new<T: ?Sized + Serialize>(v: &T, s: &Settings) -> Outcome {
    let text = ser_corpus::doc(v, s);
    let mut out = Vec::new();
    let written = oneyaml::to_writer_with(&mut out, v, s).map(|()| String::new());
    match (&text, Outcome::of(written)) {
        (Outcome::Ok(t), Outcome::Ok(_)) => assert_eq!(t.as_bytes(), out.as_slice()),
        (a @ Outcome::Err { .. }, b @ Outcome::Err { .. }) => {
            assert_eq!(*a, b);
            assert!(out.is_empty(), "a failed document wrote {out:?}");
        }
        (a, b) => panic!("to_string {a:?} but to_writer {b:?}"),
    }
    text
}

/// The error code of an outcome, if it failed.
fn code_of(o: &Outcome) -> Option<String> {
    match o {
        Outcome::Ok(_) => None,
        Outcome::Err { code, .. } => Some(code.clone()),
    }
}

/// The old multi-document stream: each document emitted from its `Value`,
/// with the old `---` rule (`Layout::SerdeYaml`: a root scalar, tag, or
/// empty collection on the marker's line; an untagged non-empty
/// collection on the next), independent of `emit::document_start`.
fn old_stream<T: Serialize>(docs: &[T], s: &Settings) -> Outcome {
    let serde_layout = s.get_emit().get_layout() == Layout::SerdeYaml;
    let mut out = String::new();
    for (i, d) in docs.iter().enumerate() {
        let v = match oneyaml::to_value_with(d, s) {
            Ok(v) => v,
            Err(e) => return Outcome::of(Err(e)),
        };
        let text = match v.to_yaml_string_with(s) {
            Ok(t) => t,
            Err(e) => return Outcome::of(Err(e)),
        };
        if i > 0 {
            let block = match &v {
                Value::Sequence(q) => !q.is_empty(),
                Value::Mapping(m) => !m.is_empty(),
                _ => false,
            };
            out.push_str(if serde_layout && !block {
                "--- "
            } else {
                "---\n"
            });
        }
        out.push_str(&text);
    }
    Outcome::Ok(out)
}

fn new_stream<T: Serialize>(docs: &[T], s: &Settings) -> Outcome {
    ser_corpus::stream(docs, s)
}

#[track_caller]
fn same<T: ?Sized + Serialize>(what: &str, v: &T, s: &Settings) -> bool {
    let (o, n) = (old(v, s), new(v, s));
    assert_eq!(n, o, "{what}");
    matches!(o, Outcome::Ok(_))
}

// ---- The tagged-variant exception ----

/// Whether the streaming path writes a newtype variant around a null, bool,
/// or number as `!Name value` under `s`: `Layout::SerdeYaml` read back with
/// `Settings::resolve_tagged_variant_content`. A `Value` holds no
/// such form (its tagged scalars are strings), so `to_value_with` keeps the
/// map form `Name: value` and the two paths differ there, and only there.
fn tagged_scalar_variants(s: &Settings) -> bool {
    s.get_emit().get_layout() == Layout::SerdeYaml && s.get_resolve_tagged_variant_content()
}

/// Whether `v` contains a newtype variant (outside map keys, which both
/// paths build as `Value`s) whose content is a null, bool, or number,
/// looking through `Some` and newtype structs as the streaming serializer
/// does.
fn has_scalar_variant<T: ?Sized + Serialize>(v: &T) -> bool {
    let found = std::cell::Cell::new(false);
    // A value whose serialization fails has no output to compare anyway.
    let _ = v.serialize(scan::Scan {
        found: &found,
        pending: false,
    });
    found.get()
}

/// The in-process comparison for the generated type set: the same bytes,
/// except for the tagged-variant exception,
/// scoped exactly to [`tagged_scalar_variants`] settings and values with a
/// scalar newtype variant. There the streaming path with the setting off
/// must equal the `Value` path (nothing else differs), with it on it must
/// differ (the exception is not wider than the change), and what it writes
/// must read back as the generated value (`back`).
#[track_caller]
fn same_generated<T: ?Sized + Serialize>(
    what: &str,
    v: &T,
    s: &Settings,
    back: &dyn Fn(&str) -> Result<(), String>,
) -> bool {
    if !(tagged_scalar_variants(s) && has_scalar_variant(v)) {
        return same(what, v, s);
    }
    let off = s.clone().resolve_tagged_variant_content(false);
    let o = old(v, s);
    assert_eq!(new(v, &off), o, "{what} (tagged variants, setting off)");
    let n = new(v, s);
    exception_output(what, &n, &o, back);
    true
}

/// [`same_generated`] for a stream of documents.
#[track_caller]
fn same_generated_stream<T: Serialize>(
    what: &str,
    docs: &[T],
    s: &Settings,
    back: &dyn Fn(&str) -> Result<(), String>,
) {
    if !(tagged_scalar_variants(s) && docs.iter().any(|d| has_scalar_variant(d))) {
        assert_eq!(new_stream(docs, s), old_stream(docs, s), "{what}");
        return;
    }
    let off = s.clone().resolve_tagged_variant_content(false);
    let o = old_stream(docs, s);
    assert_eq!(
        new_stream(docs, &off),
        o,
        "{what} (tagged variants, setting off)"
    );
    let n = new_stream(docs, s);
    exception_output(what, &n, &o, back);
}

/// The tagged-variant exception: the tagged-scalar output differs from the
/// `Value` path's and reads back as the generated value.
#[track_caller]
fn exception_output(
    what: &str,
    n: &Outcome,
    o: &Outcome,
    back: &dyn Fn(&str) -> Result<(), String>,
) {
    let (Outcome::Ok(new_text), Outcome::Ok(old_text)) = (n, o) else {
        panic!("{what} (tagged variants): {n:?} vs {o:?}");
    };
    assert_ne!(
        new_text, old_text,
        "{what} (tagged variants): no difference"
    );
    if let Err(e) = back(new_text) {
        panic!("{what} (tagged variants): {new_text:?} does not read back: {e}");
    }
}

/// A read-back check: `text` read with `s` as a `T` equals `want`.
fn reads_as<'w, T>(want: &'w T, s: &'w Settings) -> impl Fn(&str) -> Result<(), String> + 'w
where
    T: serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    move |text: &str| match oneyaml::from_str_with::<T>(text, s) {
        Ok(got) if got == *want => Ok(()),
        Ok(got) => Err(format!("read {got:?}, want {want:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// A serializer that only looks for a newtype variant around a scalar.
mod scan {
    use std::cell::Cell;
    use std::fmt;

    use serde::ser::{self, Serialize};

    #[derive(Debug)]
    pub struct Stop;

    impl fmt::Display for Stop {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("stop")
        }
    }

    impl std::error::Error for Stop {}

    impl ser::Error for Stop {
        fn custom<T: fmt::Display>(_: T) -> Self {
            Stop
        }
    }

    /// `pending`: this node is the content of a newtype variant.
    #[derive(Clone, Copy)]
    pub struct Scan<'a> {
        pub found: &'a Cell<bool>,
        pub pending: bool,
    }

    impl Scan<'_> {
        fn scalar(self) -> Result<(), Stop> {
            if self.pending {
                self.found.set(true);
            }
            Ok(())
        }
        fn inner(self) -> Self {
            Scan {
                pending: false,
                ..self
            }
        }
    }

    macro_rules! scalars {
        ($($m:ident($t:ty)),*) => {$(
            fn $m(self, _: $t) -> Result<(), Stop> { self.scalar() }
        )*};
    }

    impl<'a> ser::Serializer for Scan<'a> {
        type Ok = ();
        type Error = Stop;
        type SerializeSeq = Self;
        type SerializeTuple = Self;
        type SerializeTupleStruct = Self;
        type SerializeTupleVariant = Self;
        type SerializeMap = Self;
        type SerializeStruct = Self;
        type SerializeStructVariant = Self;

        scalars!(
            serialize_bool(bool),
            serialize_i8(i8),
            serialize_i16(i16),
            serialize_i32(i32),
            serialize_i64(i64),
            serialize_i128(i128),
            serialize_u8(u8),
            serialize_u16(u16),
            serialize_u32(u32),
            serialize_u64(u64),
            serialize_u128(u128),
            serialize_f32(f32),
            serialize_f64(f64)
        );
        fn serialize_char(self, _: char) -> Result<(), Stop> {
            Ok(())
        }
        fn serialize_str(self, _: &str) -> Result<(), Stop> {
            Ok(())
        }
        fn serialize_bytes(self, _: &[u8]) -> Result<(), Stop> {
            Ok(())
        }
        fn serialize_none(self) -> Result<(), Stop> {
            self.scalar()
        }
        fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), Stop> {
            v.serialize(self)
        }
        fn serialize_unit(self) -> Result<(), Stop> {
            self.scalar()
        }
        fn serialize_unit_struct(self, _: &'static str) -> Result<(), Stop> {
            self.scalar()
        }
        fn serialize_unit_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
        ) -> Result<(), Stop> {
            Ok(())
        }
        fn serialize_newtype_struct<T: ?Sized + Serialize>(
            self,
            _: &'static str,
            v: &T,
        ) -> Result<(), Stop> {
            v.serialize(self)
        }
        fn serialize_newtype_variant<T: ?Sized + Serialize>(
            self,
            _: &'static str,
            _: u32,
            name: &'static str,
            v: &T,
        ) -> Result<(), Stop> {
            // An empty name is always the map form.
            v.serialize(Scan {
                pending: !name.is_empty(),
                ..self
            })
        }
        fn serialize_seq(self, _: Option<usize>) -> Result<Self, Stop> {
            Ok(self.inner())
        }
        fn serialize_tuple(self, _: usize) -> Result<Self, Stop> {
            Ok(self.inner())
        }
        fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Stop> {
            Ok(self.inner())
        }
        fn serialize_tuple_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self, Stop> {
            Ok(self.inner())
        }
        fn serialize_map(self, _: Option<usize>) -> Result<Self, Stop> {
            Ok(self.inner())
        }
        fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Stop> {
            Ok(self.inner())
        }
        fn serialize_struct_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self, Stop> {
            Ok(self.inner())
        }
    }

    macro_rules! elements {
        ($($tr:ident::$m:ident),*) => {$(
            impl ser::$tr for Scan<'_> {
                type Ok = ();
                type Error = Stop;
                fn $m<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Stop> {
                    v.serialize(*self)
                }
                fn end(self) -> Result<(), Stop> {
                    Ok(())
                }
            }
        )*};
    }
    elements!(
        SerializeSeq::serialize_element,
        SerializeTuple::serialize_element,
        SerializeTupleStruct::serialize_field,
        SerializeTupleVariant::serialize_field
    );

    impl ser::SerializeMap for Scan<'_> {
        type Ok = ();
        type Error = Stop;
        // Keys are built as `Value`s by both paths: never the exception.
        fn serialize_key<T: ?Sized + Serialize>(&mut self, _: &T) -> Result<(), Stop> {
            Ok(())
        }
        fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Stop> {
            v.serialize(*self)
        }
        fn end(self) -> Result<(), Stop> {
            Ok(())
        }
    }

    macro_rules! fields {
        ($($tr:ident),*) => {$(
            impl ser::$tr for Scan<'_> {
                type Ok = ();
                type Error = Stop;
                fn serialize_field<T: ?Sized + Serialize>(
                    &mut self,
                    _: &'static str,
                    v: &T,
                ) -> Result<(), Stop> {
                    v.serialize(*self)
                }
                fn end(self) -> Result<(), Stop> {
                    Ok(())
                }
            }
        )*};
    }
    fields!(SerializeStruct, SerializeStructVariant);
}

// ---- 1. The benchmark models ----

fn models<T: Serialize>(
    name: &str,
    docs: &[T],
    sweep: &[(String, Settings)],
    counts: &mut (usize, usize),
) {
    for (label, s) in sweep {
        let (label, s) = (label.as_str(), s);
        for (i, d) in docs.iter().enumerate() {
            if same(&format!("{name} doc {i} {label}"), d, s) {
                counts.0 += 1;
            } else {
                counts.1 += 1;
            }
        }
        assert_eq!(
            new_stream(docs, s),
            old_stream(docs, s),
            "{name} stream {label}"
        );
    }
}

#[test]
fn bench_models_write_the_same_bytes() {
    let mut counts = (0, 0);
    let mut cases = Vec::new();
    let all = settings();
    let default = vec![("default".to_owned(), Settings::new())];
    // The whole sweep up to 100 KB; the 1 MB stream (k8s-multi) with the
    // defaults only, to keep the debug-build run short.
    // The baseline (`SOURCE`) was recorded before the corpus gained
    // helm-values, so it is not part of this comparison.
    for case in bench::corpus::CASES
        .iter()
        .filter(|c| c.base <= 1024 * 1024 && c.name != bench::corpus::HELM_VALUES)
    {
        let Ok(data) = prepare(SerdeLib::OneYaml, SerWorkload::Typed, case) else {
            continue;
        };
        let sweep = if case.base <= 100 * 1024 {
            &all
        } else {
            &default
        };
        cases.push(case.name);
        match &data {
            Data::K8s(d) => models(case.name, d, sweep, &mut counts),
            Data::Gha(d) => models(case.name, d, sweep, &mut counts),
            Data::Compose(d) => models(case.name, d, sweep, &mut counts),
            Data::OpenApi(d) => models(case.name, d, sweep, &mut counts),
            other => panic!("{}: unexpected data {other:?}", case.name),
        }
    }
    // Every real-world model shape, one and several documents.
    for want in [
        "k8s-manifest",
        "gha-workflow",
        "compose-merge",
        "openapi-100k",
        "k8s-multi",
    ] {
        assert!(cases.contains(&want), "{want} missing from {cases:?}");
    }
    assert!(counts.0 > 0);
    println!(
        "bench models: {} equal outputs, {} equal errors",
        counts.0, counts.1
    );
}

// ---- 2. Suite-derived `Value`s ----

#[test]
fn suite_values_write_the_same_bytes() {
    let values = suite_values();
    assert!(values.len() > 250, "{} suite values", values.len());
    let (mut ok, mut err) = (0usize, 0usize);
    for (label, s) in settings() {
        for (id, v) in &values {
            // Handed over whole, and as plain serde data.
            for r in [
                same(&format!("{id} {label} value"), v, &s),
                same(&format!("{id} {label} plain"), &Plain(v), &s),
            ] {
                if r {
                    ok += 1;
                } else {
                    err += 1;
                }
            }
        }
        let plain: Vec<Plain<'_>> = values.iter().map(|(_, v)| Plain(v)).collect();
        let handed: Vec<&Value> = values.iter().map(|(_, v)| v).collect();
        for (i, chunk) in plain.chunks(7).enumerate() {
            assert_eq!(
                new_stream(chunk, &s),
                old_stream(chunk, &s),
                "{label} plain stream chunk {i}"
            );
        }
        for (i, chunk) in handed.chunks(7).enumerate() {
            assert_eq!(
                new_stream(chunk, &s),
                old_stream(chunk, &s),
                "{label} value stream chunk {i}"
            );
        }
    }
    println!("suite values: {ok} equal outputs, {err} equal errors");
    assert!(ok > 0);
}

// ---- 3. A generated type set ----

#[test]
fn generated_types_write_the_same_bytes() {
    let all = everything();
    for (label, s) in settings() {
        // `Everything` is not `Deserialize` (and `Some(None)` does not round
        // trip in YAML): read its enum fields back through the syntax tree
        // (node reads follow `from_str_with`).
        let everything_back = |text: &str| -> Result<(), String> {
            let doc = oneyaml::Document::parse_with(text, &s).map_err(|e| e.to_string())?;
            let node = |k: &str| {
                doc.get_path(&oneyaml::path![k])
                    .ok_or_else(|| format!("no {k}"))
            };
            let enums: Vec<E> = node("enums")?.deserialize().map_err(|e| e.to_string())?;
            let keys: BTreeMap<String, E> = node("enum_keys")?
                .deserialize()
                .map_err(|e| e.to_string())?;
            if enums == all.enums && keys == all.enum_keys {
                Ok(())
            } else {
                Err(format!("read {enums:?} / {keys:?}"))
            }
        };
        assert!(
            same_generated(&format!("everything {label}"), &all, &s, &everything_back),
            "{label}"
        );
        for (i, e) in all.enums.iter().enumerate() {
            // Each variant as the document root, and nested one level.
            same_generated(&format!("enum {i} {label}"), e, &s, &reads_as(e, &s));
            let one = vec![e.clone()];
            same_generated(
                &format!("[enum {i}] {label}"),
                &[e],
                &s,
                &reads_as(&one, &s),
            );
            let some = Some(e.clone());
            same_generated(
                &format!("Some(enum {i}) {label}"),
                &Some(e),
                &s,
                &reads_as(&some, &s),
            );
            same_generated(
                &format!("{{k: enum {i}}} {label}"),
                &BTreeMap::from([("k", e)]),
                &s,
                &reads_as(&BTreeMap::from([("k".to_owned(), e.clone())]), &s),
            );
        }
        for root in [
            Value::Null,
            Value::from("x"),
            Value::from(vec![]),
            Value::from(oneyaml::Mapping::new()),
            tagged("!t", Value::from(vec![Value::from(1)])),
        ] {
            same(&format!("root {root:?} {label}"), &root, &s);
        }
        same(&format!("u128 {label}"), &u128::MAX, &s);
        same(&format!("char {label}"), &'\n', &s);
        same(&format!("str {label}"), " a\nb", &s);
        same(&format!("empty {label}"), &Vec::<u8>::new(), &s);
        // Multi-document, with every root shape after the first.
        let docs = [
            Value::from(1),
            Value::from("x"),
            Value::from(vec![]),
            Value::from(vec![Value::from(1)]),
            tagged("!t", Value::from("s")),
            tagged("!t", Value::from(vec![Value::from(1)])),
            Value::from(oneyaml::Mapping::new()),
        ];
        let plain: Vec<Plain<'_>> = docs.iter().map(Plain).collect();
        assert_eq!(new_stream(&plain, &s), old_stream(&docs, &s), "{label}");
        let enums: Vec<&E> = all.enums.iter().collect();
        same_generated_stream(&format!("enum stream {label}"), &enums, &s, &|text| {
            let docs = oneyaml::Deserializer::from_str_with(text, &s)
                .map(E::deserialize)
                .collect::<Result<Vec<E>, _>>()
                .map_err(|e| e.to_string())?;
            if docs == all.enums {
                Ok(())
            } else {
                Err(format!("read {docs:?}"))
            }
        });
    }
}

#[test]
fn errors_have_the_same_codes() {
    let deep: Value = (0..40).fold(Value::from(1), |v, _| Value::from(vec![v]));
    let bad_tag = tagged("!", Value::from("x"));
    let tagged_number = tagged("!n", Value::from(1));
    let mixed = BTreeMap::from([
        ("a", Value::from(vec![bad_tag.clone()])),
        ("b", deep.clone()),
    ]);
    let both = Value::from(vec![bad_tag.clone(), deep.clone()]);
    let (y3012, y3014, y3015, y2005, y2007) = (
        code::SERIALIZE_CUSTOM,
        code::SERIALIZE_DUPLICATE_KEY,
        code::UNREPRESENTABLE_VALUE,
        code::VALUE_DEPTH_EXCEEDED,
        code::TYPED_DEPTH_EXCEEDED,
    );
    // (settings, whether max_depth is 3, whether max_typed_depth is 3)
    for (label, s, shallow, typed) in [
        ("default", Settings::new(), false, false),
        ("depth", Settings::new().max_depth(3), true, false),
        (
            "typed",
            Settings::new().limits(Limits::default().max_typed_depth(3)),
            false,
            true,
        ),
        (
            "serde_yaml",
            Settings::new().emit(Emit::new().layout(Layout::SerdeYaml)),
            false,
            false,
        ),
    ] {
        let check = |what: &str, got: Outcome, want: Option<oneyaml::Code>| {
            assert_eq!(
                code_of(&got),
                want.map(|c| format!("Code({})", c.get())),
                "{what} {label}"
            );
        };
        let deep_want = if typed {
            Some(y2007)
        } else if shallow {
            Some(y2005)
        } else {
            None
        };
        same("fail", &FailAt(3), &s);
        check("fail", new(&FailAt(3), &s), Some(y3012));
        same("fail nested", &vec![FailAt(1)], &s);
        check("fail nested", new(&vec![FailAt(1)], &s), Some(y3012));
        same("dup", &DupKeys(Value::from("k")), &s);
        check("dup", new(&DupKeys(Value::from("k")), &s), Some(y3014));
        let complex = DupKeys(Value::from(vec![Value::from(1)]));
        same("dup complex", &complex, &s);
        check("dup complex", new(&complex, &s), Some(y3014));
        same("deep", &deep, &s);
        // A handed-over Value costs one typed level, whatever its depth.
        check("deep", new(&deep, &s), deep_want.filter(|c| *c == y2005));
        same("deep plain", &Plain(&deep), &s);
        check("deep plain", new(&Plain(&deep), &s), deep_want);
        same("bad tag", &Plain(&bad_tag), &s);
        same("bad tag value", &vec![bad_tag.clone()], &s);
        check(
            "bad tag value",
            new(&vec![bad_tag.clone()], &s),
            Some(y3015),
        );
        same("tagged number", &vec![tagged_number.clone()], &s);
        check(
            "tagged number",
            new(&vec![tagged_number.clone()], &s),
            Some(y3015),
        );
        // An output error first, a value error later: the value error.
        same("mixed", &(mixed.clone(), FailAt(0)), &s);
        check("mixed", new(&(mixed.clone(), FailAt(0)), &s), Some(y3012));
        // Y3015 before Y2005 in one value: Y2005, as the old pre-check.
        same("both handed", &both, &s);
        check(
            "both handed",
            new(&both, &s),
            Some(if shallow { y2005 } else { y3015 }),
        );
        // As plain data, `!` is the empty variant name: the mapping
        // `'': x`, no error.
        same("both plain", &Plain(&both), &s);
        check("both plain", new(&Plain(&both), &s), deep_want);
        same("unit struct", &Dup, &s);
    }
}

#[test]
fn writer_is_untouched_by_a_failed_document() {
    struct Sink(Vec<u8>);
    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.write(b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(Vec::new());
    assert!(oneyaml::to_writer(&mut sink, &FailAt(5)).is_err());
    assert!(sink.0.is_empty());
    sink.flush().unwrap();
}
