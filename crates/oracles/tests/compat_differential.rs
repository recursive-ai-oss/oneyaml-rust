//! The compat differential: `oneyaml` with
//! `Compat::serde_yaml()` against `serde_yaml_ng` 0.10.0, reading and
//! writing, over the vendored yaml-test-suite, the bench corpus (its
//! real-world and stress shapes at 1×, up to 1 MiB), and the hand-built
//! migration corpus (`compat_corpus/`: what serde_yaml users write), plus
//! string inputs: every scalar text of `scalars.tsv` and of the
//! migration corpus and the generated `ser_common::scalar_sweep`.
//!
//! For every input, each target is read (or written) by both libraries:
//!
//! | Target | Ours | Theirs |
//! |---|---|---|
//! | `v` | `from_str_with::<Value>(s, &Compat::serde_yaml())` | `from_str::<Value>` |
//! | `j` | the same into `serde_json::Value` (`deserialize_any`) | the same |
//! | `s` | each document of `Deserializer::from_str_with`, to the first error | `Deserializer::from_str` |
//! | `m` | (inputs with `<<`) `Compat::serde_yaml().merge_keys(true)` | `Value::apply_merge` |
//! | `w` | (when `v` agrees) `to_string_with(&value, &Compat::serde_yaml())` | `to_string(&value)` |
//! | `t:T` / `w:T` | typed read into `T` / write of the value read | the same |
//! | `M:T` | typed read with merge keys | `Value` → `apply_merge` → `from_value::<T>` |
//! | `S` / `S:v` | (string inputs) the text as a `String` document / mapping value, `to_string_with` | `to_string`; and it must read our bytes back as the same string |
//!
//! Two reads agree when both succeed with the same value (mapping order
//! included) or both fail; writes are compared byte for byte. Every
//! disagreement is listed in [`DIVERGENCES`] with the exact set of targets
//! that disagree and a reason from [`REASONS`]. The test fails on an
//! unlisted disagreement, on a listed one that no longer occurs (or now
//! occurs on other targets), and when a listed input breaks its reason's
//! shape (e.g. `rejects_valid` must be a spec-valid input that
//! `serde_yaml_ng` refuses and `oneyaml` reads). The output-layout
//! divergences over typed values and awkward strings are the separate,
//! equally exact list of `serde_yaml_compat.rs`.
//!
//! Also checked: every `w` output of `oneyaml` reads back to the value
//! written, and no read of the corpus hits a `Limits::default()`
//! limit (compat presets never change limits).
//!
//! `COMPAT_DIFF_PRINT=1` prints the current disagreements as table rows.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod compat_corpus;
#[path = "../../oneyaml/tests/ser_common/mod.rs"]
mod ser_common;
// The bench corpus generators (std only), shared by path: `bench` depends
// on this crate, so it cannot be a dependency of this test.
#[allow(dead_code)]
#[path = "../../bench/src/corpus.rs"]
mod bench_corpus;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use compat_corpus::Obs;
use serde::Deserialize;

/// Every input on which the libraries disagree: `(input, targets, reason)`.
/// `targets` is the exact comma-separated set of disagreeing targets, in
/// observation order.
const DIVERGENCES: &[(&str, &str, &str)] = &[
    // serde_yaml_ng refuses spec-valid YAML 1.2 (libyaml is a YAML 1.1
    // parser with known gaps; the suite marks each of these valid).
    ("2JQS", "v,j,s", "rejects_valid"),
    ("2LFX", "v,j,s", "rejects_valid"),
    ("2SXE", "v,j,s", "rejects_valid"),
    ("4ABK", "v,j,s", "rejects_valid"),
    ("4MUZ/00", "v,j,s", "rejects_valid"),
    ("4MUZ/01", "v,j,s", "rejects_valid"),
    ("4MUZ/02", "v,j,s", "rejects_valid"),
    ("58MP", "v,j,s", "rejects_valid"),
    ("5MUD", "v,j,s", "rejects_valid"),
    ("5T43", "v,j,s", "rejects_valid"),
    ("6BCT", "v,j,s", "rejects_valid"),
    ("6CA3", "v,j,s", "rejects_valid"),
    ("6LVF", "v,j,s", "rejects_valid"),
    ("6M2F", "v,j,s", "rejects_valid"),
    ("7Z25", "s", "rejects_valid"),
    ("8XYN", "v,j,s", "rejects_valid"),
    ("96NN/00", "v,j,s", "rejects_valid"),
    ("96NN/01", "v,j,s", "rejects_valid"),
    ("9SA2", "v,j,s", "rejects_valid"),
    ("A2M4", "v,j,s", "rejects_valid"),
    ("BEC7", "v,j,s", "rejects_valid"),
    ("CFD4", "v,j,s", "rejects_valid"),
    ("DBG4", "v,j,s", "rejects_valid"),
    ("DK3J", "v,j,s", "rejects_valid"),
    ("DK95/00", "v,j,s", "rejects_valid"),
    ("DK95/03", "v,j,s", "rejects_valid"),
    ("DK95/04", "v,j,s", "rejects_valid"),
    ("FH7J", "v,s", "rejects_valid"),
    ("FP8R", "v,j,s", "rejects_valid"),
    ("FRK4", "v,j,s", "rejects_valid"),
    ("HM87/00", "v,j,s", "rejects_valid"),
    ("HWV9", "v,j,s", "rejects_valid"),
    ("K3WX", "v,j,s", "rejects_valid"),
    ("M2N8/00", "v,s", "rejects_valid"),
    ("M7A3", "s", "rejects_valid"),
    ("MUS6/05", "v,j,s", "rejects_valid"),
    ("MUS6/06", "v,j,s", "rejects_valid"),
    ("NHX8", "v,j,s", "rejects_valid"),
    ("NJ66", "v,j,s", "rejects_valid"),
    ("NKF9", "s", "rejects_valid"),
    ("Q5MG", "v,j,s", "rejects_valid"),
    ("QT73", "v,j,s", "rejects_valid"),
    ("R4YG", "v,j,s", "rejects_valid"),
    ("S3PD", "v,j,s", "rejects_valid"),
    ("SM9W/01", "v,j,s", "rejects_valid"),
    ("UKK6/00", "v,j,s", "rejects_valid"),
    ("UT92", "s", "rejects_valid"),
    ("VJP3/01", "v,j,s", "rejects_valid"),
    ("W4TN", "s", "rejects_valid"),
    ("W5VH", "v,j,s", "rejects_valid"),
    ("Y79Y/001", "v,j,s", "rejects_valid"),
    ("Y79Y/010", "v,j,s", "rejects_valid"),
    ("stress:whitespace", "v,j,s", "rejects_valid"),
    ("mig:tags-core", "v,s", "rejects_valid"),
    // serde_yaml_ng reads spec-invalid input (the suite marks each an
    // error), or reports the error one document late.
    ("4H7K", "s", "accepts_invalid"),
    ("9C9N", "v,j,s", "accepts_invalid"),
    ("9HCY", "s", "accepts_invalid"),
    ("9JBA", "v,j,s", "accepts_invalid"),
    ("BS4K", "s", "accepts_invalid"),
    ("CVW2", "v,j,s", "accepts_invalid"),
    ("DK95/01", "v,j,s", "accepts_invalid"),
    ("EB22", "s", "accepts_invalid"),
    ("G5U8", "v,j,s", "accepts_invalid"),
    ("KS4U", "s", "accepts_invalid"),
    ("MUS6/00", "v,j,s", "accepts_invalid"),
    ("MUS6/01", "s", "accepts_invalid"),
    ("QB6E", "v,j,s", "accepts_invalid"),
    ("RHX7", "s", "accepts_invalid"),
    ("S98Z", "v,j,s", "accepts_invalid"),
    ("SU5Z", "v,j,s", "accepts_invalid"),
    ("X4QW", "v,j,s", "accepts_invalid"),
    ("Y79Y/003", "v,j,s", "accepts_invalid"),
    ("YJV2", "v,j,s", "accepts_invalid"),
    // Both read it; serde_yaml_ng's value is not the spec's.
    ("652Z", "v,j,s", "misreads_valid"),
    ("HM87/01", "v,j,s", "misreads_valid"),
    ("JEF9/02", "v,j,s", "misreads_valid"),
    ("L24T/01", "v,j,s", "misreads_valid"),
    ("Y2GN", "v,j,s", "misreads_valid"),
    // Tags.
    ("2XXW", "v,j,s", "tags_kept"),
    ("565N", "v,j,s", "tags_kept"),
    ("6CK3", "v,s", "tags_kept"),
    ("6WLZ", "s", "tags_kept"),
    ("9WXW", "s", "tags_kept"),
    ("C4HZ", "v,j,s", "tags_kept"),
    ("CC74", "v,j,s", "tags_kept"),
    ("J7PZ", "v,j,s", "tags_kept"),
    ("P76L", "v,j,s", "tags_kept"),
    ("QLJ7", "s", "tags_kept"),
    ("UGM3", "v,j,s", "tags_kept"),
    ("Z9M4", "v,j,s", "tags_kept"),
    ("52DL", "v,j,s", "non_specific_tag"),
    ("8MK2", "v,j,s", "non_specific_tag"),
    ("S4JQ", "v,j,s", "non_specific_tag"),
    ("UKK6/02", "v,j,s", "non_specific_tag"),
    ("mig:enum-tags", "v,s", "tagged_scalar_value"),
    ("mig:enum-maps", "t:Drawing", "enum_map_form"),
    // Streams.
    ("8G76", "s", "empty_stream"),
    ("98YD", "s", "empty_stream"),
    ("AVM7", "s", "empty_stream"),
    ("mig:comment-only", "s", "empty_stream"),
    ("mig:empty-document", "s", "empty_stream"),
    // Scalars and keys.
    ("X38W", "v,s", "duplicate_keys"),
    (
        "mig:config-duplicate-keys",
        "v,s,t:AppConfig",
        "duplicate_keys",
    ),
    ("mig:config-duplicate-map-keys", "v,s", "duplicate_keys"),
    ("mig:config-octal", "v,j,s,t:AppConfig", "leading_zero_int"),
    ("mig:config-int-keys", "v,s,t:IntKeys", "leading_zero_int"),
    ("mig:yaml-1-1-directive", "v,j,s", "leading_zero_int"),
    ("mig:config-int-forms", "v,j,s", "signed_0o"),
    ("mig:config-floats", "v,j,s", "float_overflow"),
    ("mig:config-wide-ints", "v,s", "wide_integers"),
    // Limits.
    ("stress:deep-block", "v,s", "recursion_limit"),
    // Merge keys.
    // (Their `w` differs too: a `<<` string key is quoted, see
    // `merge_key_quoted`, which `check_shape` sets apart first.)
    ("bench:compose-merge", "w,m", "merge_order"),
    ("mig:compose-merge", "w,m", "merge_order"),
    ("mig:compose-merge-list", "w,M:Compose", "merge_via_value"),
    ("mig:compose-ports", "M:Compose", "merge_via_value"),
    // Writing.
    ("7T8X", "w", "root_indicator"),
    ("9MMW", "w", "serde_yaml_write_error"),
    ("M2N8/01", "w", "serde_yaml_write_error"),
    ("V9D5", "w", "serde_yaml_write_error"),
    // Strings (targets S, S:v).
    (r#"str:"  two extra\n""#, "S", "root_indicator"),
    (
        r#"str:"+0x99999999999999999999999999999999999999999""#,
        "S,S:v",
        "core_reads_a_number",
    ),
    (
        r#"str:"-0x99999999999999999999999999999999999999999""#,
        "S,S:v",
        "core_reads_a_number",
    ),
    (
        r#"str:"0x99999999999999999999999999999999999999999""#,
        "S,S:v",
        "core_reads_a_number",
    ),
    (r#"str:"-1e400""#, "S,S:v", "core_reads_a_number"),
    (r#"str:"1e400""#, "S,S:v", "core_reads_a_number"),
    // Injection: the strings `<<` and `=` are always quoted.
    ("stress:anchors", "w", "merge_key_quoted"),
    (r#"str:"<<""#, "S,S:v", "merge_key_quoted"),
    (r#"str:"=""#, "S,S:v", "merge_key_quoted"),
];

/// The named divergences and their justification (correctness >
/// round-trip exactness > ergonomics > compatibility). Mirrored in
/// `docs/migrating-from-serde-yaml.md`.
const REASONS: &[(&str, &str)] = &[
    (
        "rejects_valid",
        "serde_yaml_ng refuses spec-valid YAML 1.2 (libyaml is a YAML 1.1 parser: tabs as \
         separation, `:` adjacent to flow keys, empty keys, unknown or 1.3 directives, bare \
         document markers, multi-line flow keys, `!!null ''`); oneyaml reads it as the \
         yaml-test-suite requires (correctness first; the suite is the authority)",
    ),
    (
        "accepts_invalid",
        "serde_yaml_ng reads input the yaml-test-suite marks invalid, or reports the error one \
         document late; oneyaml reports the error (correctness first)",
    ),
    (
        "misreads_valid",
        "both read the input, but serde_yaml_ng's value differs from the spec's (`?foo` flow \
         keys, anchors ending in `:`, trailing-space lines of block scalars); oneyaml's is the \
         suite's",
    ),
    (
        "tags_kept",
        "serde_yaml_ng keeps only `!local` tags and drops every other (`!!set`, `!!omap`, \
         `!!binary`, `%TAG`-resolved and verbatim global tags); oneyaml's `Value` keeps every \
         tag the schema does not resolve",
    ),
    (
        "non_specific_tag",
        "the non-specific tag `!` makes a scalar a string (YAML 1.2 \u{a7}6.9.1, suite 52DL, \
         S4JQ); serde_yaml_ng keeps it as a tag named `!` and resolves the content",
    ),
    (
        "tagged_scalar_value",
        "an untyped read (`Value`, a stream of `Value`s) keeps a custom-tagged scalar as the \
         tagged string (`!Square 2.0` is `Tagged(\"!Square\", \"2.0\")`: a custom tag \
         gives the schema no licence to read it as a number); serde_yaml_ng resolves the \
         content. Typed enums agree: `Compat::serde_yaml()` turns on \
         `resolve_tagged_variant_content`, which reads `!Square 2.0` into `Square(2.0)` and \
         writes it that way",
    ),
    (
        "enum_map_form",
        "serde_yaml_ng reads enum variants only from tags (`!Circle {r: 1}`) or plain unit \
         names, and refuses the externally tagged map form (`Circle: {r: 1}`) unless the field \
         uses `with::singleton_map`; oneyaml reads both forms (and, like serde_yaml, writes the \
         tag form)",
    ),
    (
        "empty_stream",
        "a stream with no document (empty, or only comments) has zero documents (YAML 1.2 \
         \u{a7}9.2, suite AVM7); serde_yaml_ng's `Deserializer` yields one null document. Single \
         document reads (`from_str`) agree: both give null",
    ),
    (
        "duplicate_keys",
        "`Compat::serde_yaml()` reads duplicate keys last-wins with a warning; \
         serde_yaml_ng refuses them in `Value` and structs (its typed maps keep the last)",
    ),
    (
        "leading_zero_int",
        "YAML 1.2 Core resolves `0644`/`010` to the decimal integers 644/10 (\u{a7}10.3.2); \
         serde_yaml reads a leading-zero decimal as a string. No setting reproduces it (open \
         design question)",
    ),
    (
        "signed_0o",
        "serde_yaml reads `-0o17` as an integer; a signed `0o` is neither YAML 1.1 nor 1.2 and \
         stays a string under every setting (`yaml11_int_forms` included)",
    ),
    (
        "float_overflow",
        "`1e400` matches the Core float pattern and reads as infinity; serde_yaml reads a float \
         that overflows as a string",
    ),
    (
        "wide_integers",
        "serde_yaml_ng's `Value` holds at most 64-bit integers and refuses wider ones; oneyaml's \
         `Number` keeps any integer",
    ),
    (
        "recursion_limit",
        "serde_yaml refuses nesting deeper than 128; oneyaml's default `max_depth` is 1,024 \
         and compat presets never change limits (`Limits::untrusted()` is 128)",
    ),
    (
        "merge_order",
        "the merged values agree; key order differs: `apply_merge` removes `<<` by swapping the \
         mapping's last key into its slot, oneyaml keeps the mapping's own keys in order and \
         appends the merged ones",
    ),
    (
        "merge_key_quoted",
        "serde_yaml writes the strings `<<` and `=` plain, which every merge-key reader \
         (PyYAML, Go, Psych, oneyaml with `merge_keys`) reads as the merge key or the YAML 1.1 \
         value key: a string key `<<` read without merging (a `Value`) would, written back, \
         merge its value into the parent (an injection). oneyaml quotes them in every \
         layout",
    ),
    (
        "merge_via_value",
        "serde_yaml's only merge mechanism goes through `Value`, whose numbers no longer read \
         into string fields; oneyaml merges while reading, so a plain `1` still reads into a \
         `String` (plain scalars read into strings as their text)",
    ),
    (
        "root_indicator",
        "a root string starting with a line break needs a block indentation indicator that \
         libyaml and YAML 1.2 count differently; oneyaml double-quotes it (as in \
         serde_yaml_compat.rs)",
    ),
    (
        "core_reads_a_number",
        "serde_yaml writes the string plain because its own reader reads a number that \
         overflows (a wide `0x` integer, `1e400`) as a string; YAML 1.2 Core (with the \
         preset's int forms), and so oneyaml, reads a number, so oneyaml quotes it (output reads back as written)",
    ),
    (
        "serde_yaml_write_error",
        "serde_yaml_ng fails to write a mapping key that is a mapping inside a sequence; oneyaml \
         writes it with `? ` and it reads back",
    ),
];

fn canon_ours(v: &oneyaml::Value, out: &mut String) {
    use oneyaml::Value as V;
    match v {
        V::Null => out.push('~'),
        V::Bool(b) => write!(out, "{b}").unwrap(),
        V::Number(n) => {
            if let Some(i) = n.as_i128() {
                write!(out, "i{i}").unwrap();
            } else if let Some(u) = n.as_u128() {
                write!(out, "i{u}").unwrap();
            } else if let Some(f) = n.as_f64() {
                float(f, out);
            } else {
                write!(out, "big{n}").unwrap();
            }
        }
        V::String(s) => write!(out, "{s:?}").unwrap(),
        V::Sequence(s) => {
            out.push('[');
            for x in s.iter() {
                canon_ours(x, out);
                out.push(',');
            }
            out.push(']');
        }
        V::Mapping(m) => {
            out.push('{');
            for (k, x) in m.iter() {
                canon_ours(k, out);
                out.push(':');
                canon_ours(x, out);
                out.push(',');
            }
            out.push('}');
        }
        V::Tagged(t) => {
            write!(out, "<{}>", t.tag).unwrap();
            canon_ours(&t.value, out);
        }
        other => panic!("unknown Value variant {other:?}"),
    }
}

fn float(f: f64, out: &mut String) {
    if f.is_nan() {
        out.push_str("fNaN");
    } else {
        write!(out, "f{f:?}").unwrap();
    }
}

fn canon_theirs(v: &serde_yaml_ng::Value, out: &mut String) {
    use serde_yaml_ng::Value as V;
    match v {
        V::Null => out.push('~'),
        V::Bool(b) => write!(out, "{b}").unwrap(),
        V::Number(n) => {
            if let Some(i) = n.as_i64() {
                write!(out, "i{i}").unwrap();
            } else if let Some(u) = n.as_u64() {
                write!(out, "i{u}").unwrap();
            } else if let Some(f) = n.as_f64() {
                float(f, out);
            }
        }
        V::String(s) => write!(out, "{s:?}").unwrap(),
        V::Sequence(s) => {
            out.push('[');
            for x in s {
                canon_theirs(x, out);
                out.push(',');
            }
            out.push(']');
        }
        V::Mapping(m) => {
            out.push('{');
            for (k, x) in m {
                canon_theirs(k, out);
                out.push(':');
                canon_theirs(x, out);
                out.push(',');
            }
            out.push('}');
        }
        V::Tagged(t) => {
            write!(out, "<{}>", t.tag).unwrap();
            canon_theirs(&t.value, out);
        }
    }
}

fn co(v: &oneyaml::Value) -> String {
    let mut o = String::new();
    canon_ours(v, &mut o);
    o
}

fn ct(v: &serde_yaml_ng::Value) -> String {
    let mut o = String::new();
    canon_theirs(v, &mut o);
    o
}

fn compat() -> oneyaml::Settings {
    oneyaml::Compat::serde_yaml()
}

/// A read of the real-world corpus (the suite, the bench corpus's
/// real-world shapes, the migration corpus) never hits a
/// `Limits::default()` limit; the compat preset keeps the default limits.
/// The bench stress shapes are exempt: `deep-block` is deeper than the
/// default `max_typed_depth` (128) on purpose.
fn no_limit<T>(id: &str, r: &Result<T, oneyaml::Error>) {
    if id.starts_with("stress:") {
        return;
    }
    if let Err(e) = r {
        assert_ne!(
            e.code().range(),
            oneyaml::CodeRange::Limits,
            "{id}: hit a default limit: {e}"
        );
    }
}

/// Every observation for one input.
fn observe(id: &str, src: &str, typed: fn(&str) -> Vec<Obs>) -> Vec<Obs> {
    let mut out = Vec::new();
    // v: `Value`, then w: each library writes the `Value` it read.
    let ours = oneyaml::from_str_with::<oneyaml::Value>(src, &compat());
    no_limit(id, &ours);
    let theirs = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(src);
    let v = Obs {
        target: "v".to_owned(),
        ours: ours.as_ref().map(co).map_err(|e| e.to_string()),
        theirs: theirs.as_ref().map(ct).map_err(|e| e.to_string()),
    };
    let write = match (v.agrees(), &ours, &theirs) {
        (true, Ok(a), Ok(b)) => {
            let written = oneyaml::to_string_with(a, &compat());
            if let Ok(text) = &written {
                // What we write reads back.
                let back: oneyaml::Value = oneyaml::from_str_with(text, &compat())
                    .unwrap_or_else(|e| panic!("{id}: our output does not read back: {e}"));
                assert_eq!(&back, a, "{id}: our output reads back differently");
            }
            Some(Obs {
                target: "w".to_owned(),
                ours: written.map_err(|e| e.to_string()),
                theirs: serde_yaml_ng::to_string(b).map_err(|e| e.to_string()),
            })
        }
        _ => None,
    };
    out.push(v);
    out.extend(write);
    // j: a typed target read through `deserialize_any`.
    let ours_j = oneyaml::from_str_with::<serde_json::Value>(src, &compat());
    no_limit(id, &ours_j);
    out.push(Obs {
        target: "j".to_owned(),
        ours: ours_j.map(|v| v.to_string()).map_err(|e| e.to_string()),
        theirs: serde_yaml_ng::from_str::<serde_json::Value>(src)
            .map(|v| v.to_string())
            .map_err(|e| e.to_string()),
    });
    out.push(stream(id, src));
    // m: merge keys on (ours) / `apply_merge` (theirs).
    if src.contains("<<") {
        out.push(merged(id, src));
    }
    out.extend(typed(src));
    out
}

/// s: every document of the stream, up to the first error.
fn stream(id: &str, src: &str) -> Obs {
    let mut ours = Vec::new();
    for d in oneyaml::Deserializer::from_str_with(src, &compat()) {
        let r = oneyaml::Value::deserialize(d);
        no_limit(id, &r);
        match r {
            Ok(v) => ours.push(co(&v)),
            Err(_) => {
                ours.push("Err".to_owned());
                break;
            }
        }
    }
    let mut theirs = Vec::new();
    for d in serde_yaml_ng::Deserializer::from_str(src) {
        match serde_yaml_ng::Value::deserialize(d) {
            Ok(v) => theirs.push(ct(&v)),
            Err(_) => {
                theirs.push("Err".to_owned());
                break;
            }
        }
    }
    Obs {
        target: "s".to_owned(),
        ours: Ok(ours.join(" | ")),
        theirs: Ok(theirs.join(" | ")),
    }
}

fn merged(id: &str, src: &str) -> Obs {
    let ours = oneyaml::from_str_with::<oneyaml::Value>(src, &compat().merge_keys(true));
    no_limit(id, &ours);
    let theirs = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(src).and_then(|mut v| {
        v.apply_merge()?;
        Ok(v)
    });
    Obs {
        target: "m".to_owned(),
        ours: ours.as_ref().map(co).map_err(|e| e.to_string()),
        theirs: theirs.as_ref().map(ct).map_err(|e| e.to_string()),
    }
}

fn no_typed(_: &str) -> Vec<Obs> {
    Vec::new()
}

struct Source {
    id: String,
    src: String,
    /// `Some(expect_error)` for a yaml-test-suite case.
    suite_error: Option<bool>,
    /// A string-write input: `src` is the text of one string, written by
    /// both libraries (targets `S`, `S:v`), not a YAML document.
    string: bool,
    typed: fn(&str) -> Vec<Obs>,
}

fn sources() -> Vec<Source> {
    let mut out = Vec::new();
    for c in test_suite::load_suite(&test_suite::default_data_dir()).unwrap() {
        let src = String::from_utf8(c.input.clone()).expect("the suite is UTF-8");
        out.push(Source {
            id: c.id.clone(),
            src,
            suite_error: Some(c.expect_error),
            string: false,
            typed: no_typed,
        });
    }
    for c in bench_corpus::CASES.iter().filter(|c| c.base <= 1024 * 1024) {
        out.push(Source {
            id: match c.kind {
                bench_corpus::Kind::RealWorld => format!("bench:{}", c.name),
                _ => format!("stress:{}", c.name),
            },
            src: c.generate(1),
            suite_error: None,
            string: false,
            typed: no_typed,
        });
    }
    for i in compat_corpus::INPUTS {
        out.push(Source {
            id: format!("mig:{}", i.name),
            src: i.src.to_owned(),
            suite_error: None,
            string: false,
            typed: i.typed,
        });
    }
    for text in string_texts() {
        out.push(Source {
            id: format!("str:{text:?}"),
            src: text,
            suite_error: None,
            string: true,
            typed: no_typed,
        });
    }
    out
}

/// The texts of the string-write targets: every scalar of
/// `data/pyyaml-1.1/scalars.tsv`, every scalar text of the migration corpus
/// (read with the Failsafe schema, so numbers and booleans are their text),
/// and the generated sweep of `ser_common::scalar_sweep`.
fn string_texts() -> Vec<String> {
    let mut out: Vec<String> = ser_common::scalar_sweep();
    let tsv = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/pyyaml-1.1/scalars.tsv");
    let tsv = std::fs::read_to_string(tsv).unwrap();
    out.extend(
        tsv.lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split('\t').next())
            .map(str::to_owned),
    );
    let failsafe = oneyaml::Settings::new().schema(oneyaml::Schema::Failsafe);
    for i in compat_corpus::INPUTS {
        for d in oneyaml::Deserializer::from_str_with(i.src, &failsafe) {
            if let Ok(v) = oneyaml::Value::deserialize(d) {
                collect_strings(&v, &mut out);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_strings(v: &oneyaml::Value, out: &mut Vec<String>) {
    use oneyaml::Value as V;
    match v {
        V::String(s) => out.push(s.clone()),
        V::Sequence(s) => s.iter().for_each(|x| collect_strings(x, out)),
        V::Mapping(m) => m.iter().for_each(|(k, x)| {
            collect_strings(k, out);
            collect_strings(x, out);
        }),
        V::Tagged(t) => collect_strings(&t.value, out),
        _ => {}
    }
}

/// S / S:v: `text` written as a string (a document, and a mapping value) by
/// both libraries, bytes compared. Whatever we write must read back as the
/// same string in serde_yaml_ng too (and in oneyaml under the preset).
fn observe_string(id: &str, text: &str) -> Vec<Obs> {
    let map = BTreeMap::from([("k", text)]);
    let pairs = [
        (
            "S",
            oneyaml::to_string_with(&text, &compat()),
            serde_yaml_ng::to_string(&text),
        ),
        (
            "S:v",
            oneyaml::to_string_with(&map, &compat()),
            serde_yaml_ng::to_string(&map),
        ),
    ];
    let mut out = Vec::new();
    for (target, ours, theirs) in pairs {
        let ours = ours.unwrap_or_else(|e| panic!("{id} {target}: we fail to write it: {e}"));
        let want = if target == "S" {
            serde_yaml_ng::Value::String(text.to_owned())
        } else {
            serde_yaml_ng::to_value(&map).unwrap()
        };
        let read = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&ours);
        assert!(
            read.as_ref().is_ok_and(|r| *r == want),
            "{id} {target}: serde_yaml_ng reads our {ours:?} as {read:?}"
        );
        let back: oneyaml::Value = oneyaml::from_str_with(&ours, &compat()).unwrap();
        assert_eq!(
            back,
            oneyaml::to_value(&map)
                .map(|m| if target == "S" {
                    oneyaml::Value::from(text)
                } else {
                    m
                })
                .unwrap(),
            "{id} {target}"
        );
        out.push(Obs {
            target: target.to_owned(),
            ours: Ok(ours),
            theirs: theirs.map_err(|e| e.to_string()),
        });
    }
    out
}

/// `Ok` text of an observation, or "".
fn ok(r: &Result<String, String>) -> &str {
    r.as_deref().unwrap_or("")
}

/// The integers of a canonical rendering: the `i<digits>` tokens between
/// structural characters.
fn ints(canon: &str) -> Vec<String> {
    canon
        .split(['[', ']', '{', '}', ',', ':', '>'])
        .filter_map(|t| t.strip_prefix('i'))
        .filter(|d| {
            let digits = d.strip_prefix('-').unwrap_or(d);
            !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .collect()
}

/// The quoted strings of a canonical rendering that are all digits with a
/// leading zero (`"0644"`).
fn leading_zero_strings(canon: &str) -> Vec<String> {
    canon
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|t| t.len() > 1 && t.starts_with('0') && t.bytes().all(|c| c.is_ascii_digit()))
        .map(str::to_owned)
        .collect()
}

/// Our reading of a suite case equals the suite's own `in.json` (every
/// document), when the case has one: for `rejects_valid`/`misreads_valid`
/// the spec's value is ours, not just "we read something".
fn matches_suite_json(s: &Source) -> Result<(), String> {
    let path = test_suite::default_data_dir().join(&s.id).join("in.json");
    let Ok(json) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let expected = serde_json::Deserializer::from_str(&json)
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("in.json: {e}"))?;
    let ours = oneyaml::Deserializer::from_str_with(&s.src, &compat())
        .map(serde_json::Value::deserialize)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("ours does not read as JSON: {e}"))?;
    if ours == expected {
        Ok(())
    } else {
        Err(format!("ours {ours:?} != in.json {expected:?}"))
    }
}

/// The shape a reason requires of its inputs, so a disagreement cannot be
/// filed under a reason that does not describe it. Every reason has one.
/// A write that differs only by quoting the strings `<<` and `=`
/// (reason `merge_key_quoted`).
fn quoted_merge_key(o: &Obs) -> bool {
    let (ours, theirs) = (ok(&o.ours), ok(&o.theirs));
    o.ours.is_ok()
        && o.theirs.is_ok()
        && ours != theirs
        && ours.replace("'<<'", "<<").replace("'='", "=") == theirs
}

fn check_shape(reason: &str, s: &Source, bad: &[&Obs]) -> Result<(), String> {
    if reason == "merge_key_quoted" {
        return if !bad.is_empty() && bad.iter().all(|o| quoted_merge_key(o)) {
            Ok(())
        } else {
            Err("not a write that differs only by quoting `<<` or `=`".to_owned())
        };
    }
    // Such writes may come with another reason's differences: set apart.
    let rest: Vec<&Obs> = bad
        .iter()
        .copied()
        .filter(|o| !quoted_merge_key(o))
        .collect();
    let bad = rest.as_slice();
    let refuses = |o: &&Obs| match o.target.as_str() {
        "s" => {
            let (a, b) = (o.ours.as_deref().unwrap(), o.theirs.as_deref().unwrap());
            b.contains("Err") && !a.contains("Err")
        }
        _ => o.ours.is_ok() && o.theirs.is_err(),
    };
    let v = bad.iter().find(|o| o.target == "v");
    let fail = |m: &str| Err(m.to_owned());
    match reason {
        "rejects_valid" => {
            if s.suite_error == Some(true) {
                return fail("an invalid suite case");
            }
            if !bad.iter().all(refuses) {
                return fail("not every target is ours Ok / theirs Err");
            }
            matches_suite_json(s)?;
        }
        "accepts_invalid" => {
            if s.suite_error != Some(true) {
                return fail("not an invalid suite case");
            }
            if bad
                .iter()
                .any(|o| o.target != "s" && (o.ours.is_ok() || o.theirs.is_err()))
            {
                return fail("a target is not ours Err / theirs Ok");
            }
        }
        "misreads_valid" => {
            if s.suite_error != Some(false) || bad.iter().any(|o| o.theirs.is_err()) {
                return fail("not a valid suite case both libraries read");
            }
            matches_suite_json(s)?;
        }
        "tags_kept" => {
            // Ours keeps a global tag (`<tag:...>`) theirs dropped.
            if !bad.iter().any(|o| {
                ok(&o.ours).matches("<tag:").count() > ok(&o.theirs).matches("<tag:").count()
            }) {
                return fail("ours keeps no tag theirs dropped");
            }
        }
        "non_specific_tag" => {
            // Theirs keeps a tag named `!` (rendered `<!!>`); ours has none.
            if !bad
                .iter()
                .any(|o| ok(&o.theirs).contains("<!!>") && !ok(&o.ours).contains("<!!>"))
            {
                return fail("theirs has no `!` tag");
            }
        }
        "tagged_scalar_value" => {
            // Theirs resolves a local-tagged scalar to a number; ours keeps
            // the tagged string, in untyped reads only.
            if bad.iter().any(|o| o.target != "v" && o.target != "s") {
                return fail("a typed target disagrees");
            }
            let Some(v) = v else {
                return fail("no v target");
            };
            let (a, b) = (ok(&v.ours), ok(&v.theirs));
            let tagged_num = |t: &str| {
                t.split("<!").skip(1).any(|r| {
                    r.split_once('>')
                        .is_some_and(|(_, c)| c.starts_with(['i', 'f']))
                })
            };
            let tagged_str = |t: &str| {
                t.split("<!")
                    .skip(1)
                    .any(|r| r.split_once('>').is_some_and(|(_, c)| c.starts_with('"')))
            };
            if !(tagged_num(b) && !tagged_num(a) && tagged_str(a)) {
                return fail("no local-tagged number vs tagged string");
            }
        }
        "enum_map_form" => {
            if !bad.iter().any(|o| {
                o.target.starts_with("t:")
                    && o.ours.is_ok()
                    && o.theirs
                        .as_ref()
                        .is_err_and(|e| e.contains("expected a YAML tag starting with '!'"))
            }) {
                return fail("theirs does not refuse the map form");
            }
        }
        "empty_stream" => {
            if bad
                .iter()
                .any(|o| o.target != "s" || !ok(&o.ours).is_empty() || ok(&o.theirs) != "~")
            {
                return fail("not zero documents (ours) vs one null (theirs)");
            }
        }
        "duplicate_keys" => {
            let dup = oneyaml::from_str::<oneyaml::Value>(&s.src).map_err(|e| e.code());
            if dup != Err(oneyaml::code::DUPLICATE_KEY) {
                return Err(format!(
                    "default settings do not refuse a duplicate: {dup:?}"
                ));
            }
            if !bad.iter().all(refuses) {
                return fail("not every target is ours Ok / theirs Err");
            }
        }
        "leading_zero_int" => {
            // Theirs has `"0644"` where ours has the Core integer 644.
            let Some(v) = v else {
                return fail("no v target");
            };
            let ours = ints(ok(&v.ours));
            if !leading_zero_strings(ok(&v.theirs)).iter().any(|z| {
                let n = z.trim_start_matches('0');
                ours.iter().any(|i| i == if n.is_empty() { "0" } else { n })
            }) {
                return fail("no leading-zero string read as an integer");
            }
        }
        "signed_0o" => {
            let Some(v) = v else {
                return fail("no v target");
            };
            if !(ok(&v.ours).contains("\"-0o") && !ok(&v.theirs).contains("\"-0o")) {
                return fail("no signed 0o string on our side");
            }
        }
        "float_overflow" => {
            let Some(v) = v else {
                return fail("no v target");
            };
            let overflowed = ok(&v.theirs)
                .split('"')
                .skip(1)
                .step_by(2)
                .any(|t| t.parse::<f64>().is_ok_and(f64::is_infinite));
            if !(ok(&v.ours).contains("finf") && overflowed) {
                return fail("no overflowing float read as infinity");
            }
        }
        "wide_integers" => {
            let Some(v) = v else {
                return fail("no v target");
            };
            let wide = ints(ok(&v.ours))
                .iter()
                .any(|i| i.parse::<i64>().is_err() && i.parse::<u64>().is_err())
                || ok(&v.ours).contains("big");
            if !(wide && v.theirs.is_err()) {
                return fail("no integer outside 64 bits that theirs refuses");
            }
        }
        "recursion_limit" => {
            let depth = oneyaml::from_str_with::<oneyaml::Value>(&s.src, &compat())
                .map(|v| v.depth())
                .unwrap_or(0);
            let limit = v.is_some_and(|v| {
                v.theirs
                    .as_ref()
                    .is_err_and(|e| e.contains("recursion limit exceeded"))
            });
            if !((129..=1024).contains(&depth) && limit && bad.iter().all(refuses)) {
                return Err(format!(
                    "depth {depth}, or theirs is not its recursion limit"
                ));
            }
        }
        "merge_order" => {
            // Only `m` differs, and only in order: the merged data agree.
            let ours = oneyaml::from_str_with::<oneyaml::Value>(&s.src, &compat().merge_keys(true))
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()));
            let theirs = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&s.src)
                .and_then(|mut v| {
                    v.apply_merge()?;
                    Ok(v)
                })
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()));
            let unordered_equal = matches!((&ours, &theirs), (Ok(a), Ok(b)) if a == b);
            if bad.iter().any(|o| o.target != "m") || !unordered_equal {
                return fail("the merged data differ beyond key order");
            }
        }
        "merge_via_value" => {
            if !bad.iter().all(|o| {
                o.target.starts_with("M:")
                    && o.ours.is_ok()
                    && o.theirs
                        .as_ref()
                        .is_err_and(|e| e.contains("expected a string"))
            }) {
                return fail("not a typed merge refused by from_value for a string field");
            }
        }
        "core_reads_a_number" => {
            let number = oneyaml::from_str_with::<oneyaml::Value>(&s.src, &compat())
                .is_ok_and(|v| v.is_number());
            let quoted = bad.iter().all(|o| {
                let text = ok(&o.ours);
                text.strip_prefix("k: ").unwrap_or(text).starts_with('\'')
                    && !ok(&o.theirs).contains('\'')
            });
            if !(s.string && number && quoted) {
                return fail("not a string Core reads as a number, quoted by us only");
            }
        }
        "root_indicator" => {
            let w = bad.iter().find(|o| o.target == "w" || o.target == "S");
            if !w.is_some_and(|w| {
                ok(&w.ours).starts_with('"') && ok(&w.theirs).starts_with(['|', '>'])
            }) {
                return fail("not a double-quoted root vs a block scalar with an indicator");
            }
        }
        "serde_yaml_write_error" => {
            if !bad
                .iter()
                .all(|o| o.target == "w" && o.ours.is_ok() && o.theirs.is_err())
            {
                return fail("not a write theirs fails");
            }
        }
        other => return Err(format!("no shape for reason {other}")),
    }
    Ok(())
}

#[test]
fn compat_preset_reads_and_writes_like_serde_yaml_ng_except_the_listed_divergences() {
    // Deep inputs (bench stress shapes) are canonicalised recursively here.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let mut listed: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for &(id, targets, reason) in DIVERGENCES {
        assert!(
            listed.insert(id, (targets, reason)).is_none(),
            "{id} is listed twice"
        );
        assert!(
            REASONS.iter().any(|(r, _)| *r == reason),
            "{id}: unknown reason {reason}"
        );
    }
    for (r, _) in REASONS {
        assert!(
            DIVERGENCES.iter().any(|d| d.2 == *r),
            "reason {r} has no input"
        );
    }
    let print = std::env::var_os("COMPAT_DIFF_PRINT").is_some();
    let mut report = Vec::new();
    let mut seen = Vec::new();
    let mut inputs = 0usize;
    let mut strings = 0usize;
    let mut observations = 0usize;
    for s in sources() {
        inputs = inputs.saturating_add(1);
        strings = strings.saturating_add(usize::from(s.string));
        let obs = if s.string {
            observe_string(&s.id, &s.src)
        } else {
            observe(&s.id, &s.src, s.typed)
        };
        observations = observations.saturating_add(obs.len());
        let bad: Vec<&Obs> = obs.iter().filter(|o| !o.agrees()).collect();
        let targets = bad
            .iter()
            .map(|o| o.target.as_str())
            .collect::<Vec<_>>()
            .join(",");
        if print && !bad.is_empty() {
            println!("    (\"{}\", \"{targets}\", \"?\"),", s.id);
        }
        match (bad.is_empty(), listed.get(s.id.as_str())) {
            (true, None) => {}
            (true, Some(_)) => report.push(format!("{}: listed but now agrees", s.id)),
            (false, None) => {
                let mut msg = format!("{} [{targets}]: disagrees (unlisted)", s.id);
                for o in &bad {
                    let _ = write!(
                        msg,
                        "\n  {} ours:   {:.300}\n  {} theirs: {:.300}",
                        o.target,
                        format!("{:?}", o.ours),
                        o.target,
                        format!("{:?}", o.theirs)
                    );
                }
                report.push(msg);
            }
            (false, Some((want, reason))) => {
                seen.push(s.id.clone());
                if *want != targets {
                    report.push(format!(
                        "{}: listed on [{want}] but disagrees on [{targets}]",
                        s.id
                    ));
                }
                if let Err(e) = check_shape(reason, &s, &bad) {
                    report.push(format!("{}: does not fit {reason}: {e}", s.id));
                }
            }
        }
    }
    for id in listed.keys() {
        if !seen.iter().any(|s| s == id) && !report.iter().any(|r| r.starts_with(id)) {
            report.push(format!("{id}: listed but not an input"));
        }
    }
    // The corpus really is broad: the suite, the bench corpus, and the
    // migration corpus, with typed targets.
    assert!(inputs > 1000, "{inputs} inputs");
    assert!(strings > 450, "{strings} string-write inputs");
    assert!(observations > 1600, "{observations} observations");
    assert!(
        report.is_empty(),
        "{} problems:\n{}",
        report.len(),
        report.join("\n")
    );
}
