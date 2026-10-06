//! Serde comparators for the bench crate's `serde` sections (serde
//! paths ≤ 1.25× the fastest YAML comparator).
//!
//! Each library is driven through its own public entry points, with its
//! defaults, doing the same work as `oneyaml`: every document of a stream
//! (with the library's multi-document API), and merge keys (`<<`) expanded
//! when the caller asks for them, with the library's own mechanism. Where a
//! library has no way to do the equivalent work, the call returns
//! [`Failure::Unsupported`] instead of silently doing less.
//!
//! | Library | Documents | Merge keys | Dynamic value |
//! |---|---|---|---|
//! | serde_yaml_ng 0.10.0 | `Deserializer::from_str` iterator | only `Value::apply_merge`, so typed reads go `Value` → `apply_merge` → `from_value` | `serde_yaml_ng::Value` |
//! | serde_norway 0.9.42 | as serde_yaml_ng | as serde_yaml_ng | `serde_norway::Value` |
//! | serde_yaml2 0.1.3 | first document only: [`Failure::Unsupported`] | none: [`Failure::Unsupported`] | `yaml_rust2::Yaml` (its `YamlNodeWrapper`) |
//! | serde-saphyr 1.3.0 | `from_multiple` | expanded by default (`MergeKeyPolicy::Merge`) | none of its own: `serde_json::Value` |
//! | serde_json | the JSON input is an array of the documents | already expanded in the JSON | `serde_json::Value` |
//!
//! serde-saphyr runs with [`saphyr_options`]: its defaults with the three
//! budget caps that reject k8s-multi and openapi-10m lifted.
//!
//! The write direction ([`write()`], [`write_dyn`]) serializes every
//! document into one YAML stream with each library's own writer and its
//! defaults:
//!
//! | Library | One document | Several documents |
//! |---|---|---|
//! | serde_yaml_ng, serde_norway | `to_string` | one `Serializer` over a `Vec<u8>`, `serialize` per document (it writes the `---` separators) |
//! | serde_yaml2 | `to_string` | none: [`Failure::Unsupported`] |
//! | serde-saphyr | `to_string` | `to_string_multiple` |
//! | serde_json | `to_string` | `to_string` of the array of documents |

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A serde comparator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Lib {
    /// `serde_yaml_ng` 0.10.0 (the maintained `serde_yaml` fork, libyaml-based).
    YamlNg,
    /// `serde_norway` 0.9.42 (another `serde_yaml` fork).
    Norway,
    /// `serde_yaml2` 0.1.3 (serde over yaml-rust2).
    Yaml2,
    /// `serde-saphyr` 1.3.0 (serde over a saphyr-parser fork).
    Saphyr,
    /// `serde_json` on the equivalent JSON: a reference ceiling, not a YAML
    /// comparator.
    Json,
}

impl Lib {
    /// Every comparator, YAML libraries first.
    pub const ALL: &'static [Lib] = &[Lib::YamlNg, Lib::Norway, Lib::Yaml2, Lib::Saphyr, Lib::Json];

    /// The YAML serde comparators (the "fastest comparator" of the serde target
    /// is among these).
    pub const YAML: &'static [Lib] = &[Lib::YamlNg, Lib::Norway, Lib::Yaml2, Lib::Saphyr];

    /// Stable command-line / table name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Lib::YamlNg => "serde_yaml_ng",
            Lib::Norway => "serde_norway",
            Lib::Yaml2 => "serde_yaml2",
            Lib::Saphyr => "serde-saphyr",
            Lib::Json => "serde_json",
        }
    }

    /// Pinned version, as in `Cargo.toml`.
    #[must_use]
    pub const fn version(self) -> &'static str {
        match self {
            Lib::YamlNg => "0.10.0",
            Lib::Norway => "0.9.42",
            Lib::Yaml2 => "0.1.3",
            Lib::Saphyr => "1.3.0",
            Lib::Json => "1.0.151",
        }
    }

    /// Looks a library up by [`Lib::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Lib> {
        Lib::ALL.iter().copied().find(|l| l.name() == name)
    }

    /// A feature-complete YAML serde library in the sense of the serde target
    /// (aliases, multi-document streams, merge keys): the comparators the
    /// serde target (≤ 1.25× the fastest) is gated against. serde_yaml2
    /// (no multi-document API, no merge keys) is partial: reported, not
    /// gated; serde_json is not a YAML library.
    #[must_use]
    pub const fn feature_complete(self) -> bool {
        matches!(self, Lib::YamlNg | Lib::Norway | Lib::Saphyr)
    }
}

/// What the input needs for the work to be equivalent to ours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    /// The input is a stream of several documents; read every one.
    pub multi: bool,
    /// Expand `<<` merge keys.
    pub merge: bool,
}

/// Why a call produced nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Failure {
    /// The library cannot do the equivalent work (no multi-document API, no
    /// merge keys); the reason.
    Unsupported(&'static str),
    /// The library reported an error.
    Rejected(String),
}

/// The library's `Unsupported` reason, if it can't do `opts` at all.
#[must_use]
pub const fn unsupported(lib: Lib, opts: Opts) -> Option<&'static str> {
    match lib {
        Lib::Yaml2 if opts.multi => Some("no multi-document API (reads the first document only)"),
        Lib::Yaml2 if opts.merge => Some("no merge-key support"),
        _ => None,
    }
}

fn rejected(e: impl std::fmt::Display) -> Failure {
    Failure::Rejected(e.to_string())
}

/// A library's own dynamic value, one per document.
#[derive(Debug)]
#[non_exhaustive]
pub enum Dyn {
    /// `serde_yaml_ng::Value`s.
    YamlNg(Vec<serde_yaml_ng::Value>),
    /// `serde_norway::Value`s.
    Norway(Vec<serde_norway::Value>),
    /// `yaml_rust2::Yaml` in serde_yaml2's wrapper.
    Yaml2(Vec<serde_yaml2::wrapper::YamlNodeWrapper>),
    /// `serde_json::Value`s (serde-saphyr and serde_json).
    Json(Vec<serde_json::Value>),
}

impl Dyn {
    /// Number of documents.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Dyn::YamlNg(v) => v.len(),
            Dyn::Norway(v) => v.len(),
            Dyn::Yaml2(v) => v.len(),
            Dyn::Json(v) => v.len(),
        }
    }

    /// No documents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every document as a `serde_json::Value`, for the correctness guard.
    ///
    /// # Errors
    /// A value JSON can't hold (e.g. a non-string mapping key).
    pub fn to_json(&self) -> Result<Vec<serde_json::Value>, String> {
        fn each<T: serde::Serialize>(v: &[T]) -> Result<Vec<serde_json::Value>, String> {
            v.iter()
                .map(|d| serde_json::to_value(d).map_err(|e| e.to_string()))
                .collect()
        }
        match self {
            Dyn::YamlNg(v) => each(v),
            Dyn::Norway(v) => each(v),
            Dyn::Yaml2(v) => each(v),
            Dyn::Json(v) => Ok(v.clone()),
        }
    }
}

/// Reads every document of `input` into `T` (for [`Lib::Json`], `input` is
/// JSON: an array of the documents when `opts.multi`, else the document).
///
/// # Errors
/// [`Failure::Unsupported`] when the library can't do the equivalent work,
/// [`Failure::Rejected`] when it reports an error.
pub fn typed<T: DeserializeOwned>(lib: Lib, input: &str, opts: Opts) -> Result<Vec<T>, Failure> {
    if let Some(why) = unsupported(lib, opts) {
        return Err(Failure::Unsupported(why));
    }
    match lib {
        Lib::YamlNg => {
            if opts.merge {
                let values = ng_values(input, opts)?;
                return values
                    .into_iter()
                    .map(|v| serde_yaml_ng::from_value(v).map_err(rejected))
                    .collect();
            }
            if opts.multi {
                return serde_yaml_ng::Deserializer::from_str(input)
                    .map(|d| T::deserialize(d).map_err(rejected))
                    .collect();
            }
            serde_yaml_ng::from_str(input)
                .map(|t| vec![t])
                .map_err(rejected)
        }
        Lib::Norway => {
            if opts.merge {
                let values = norway_values(input, opts)?;
                return values
                    .into_iter()
                    .map(|v| serde_norway::from_value(v).map_err(rejected))
                    .collect();
            }
            if opts.multi {
                return serde_norway::Deserializer::from_str(input)
                    .map(|d| T::deserialize(d).map_err(rejected))
                    .collect();
            }
            serde_norway::from_str(input)
                .map(|t| vec![t])
                .map_err(rejected)
        }
        Lib::Yaml2 => serde_yaml2::from_str(input)
            .map(|t| vec![t])
            .map_err(rejected),
        Lib::Saphyr => {
            if opts.multi {
                serde_saphyr::from_multiple_with_options(input, saphyr_options()).map_err(rejected)
            } else {
                serde_saphyr::from_str_with_options(input, saphyr_options())
                    .map(|t| vec![t])
                    .map_err(rejected)
            }
        }
        Lib::Json => {
            if opts.multi {
                serde_json::from_str(input).map_err(rejected)
            } else {
                serde_json::from_str(input)
                    .map(|t| vec![t])
                    .map_err(rejected)
            }
        }
    }
}

/// serde-saphyr's defaults, except the three budget caps the corpus exceeds
/// (1,024 documents, 250,000 nodes, 1,000,000 events: k8s-multi and
/// openapi-10m), which are lifted so it does the same work as ours under
/// `Limits::default()`. Every other budget check still runs.
#[must_use]
pub fn saphyr_options() -> serde_saphyr::Options {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_documents = usize::MAX;
    budget.max_nodes = usize::MAX;
    budget.max_events = usize::MAX;
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    options
}

fn ng_values(input: &str, opts: Opts) -> Result<Vec<serde_yaml_ng::Value>, Failure> {
    let mut values: Vec<serde_yaml_ng::Value> = if opts.multi {
        serde_yaml_ng::Deserializer::from_str(input)
            .map(|d| serde::Deserialize::deserialize(d).map_err(rejected))
            .collect::<Result<_, _>>()?
    } else {
        vec![serde_yaml_ng::from_str(input).map_err(rejected)?]
    };
    if opts.merge {
        for v in &mut values {
            v.apply_merge().map_err(rejected)?;
        }
    }
    Ok(values)
}

fn norway_values(input: &str, opts: Opts) -> Result<Vec<serde_norway::Value>, Failure> {
    let mut values: Vec<serde_norway::Value> = if opts.multi {
        serde_norway::Deserializer::from_str(input)
            .map(|d| serde::Deserialize::deserialize(d).map_err(rejected))
            .collect::<Result<_, _>>()?
    } else {
        vec![serde_norway::from_str(input).map_err(rejected)?]
    };
    if opts.merge {
        for v in &mut values {
            v.apply_merge().map_err(rejected)?;
        }
    }
    Ok(values)
}

/// The library's `Unsupported` reason, if it can't write `opts` at all.
#[must_use]
pub const fn write_unsupported(lib: Lib, opts: Opts) -> Option<&'static str> {
    match lib {
        Lib::Yaml2 if opts.multi => Some("no multi-document API (writes one document only)"),
        _ => None,
    }
}

fn one<T>(docs: &[T]) -> Result<&T, Failure> {
    match docs {
        [d] => Ok(d),
        _ => Err(Failure::Rejected(format!(
            "{} documents for a single-document write",
            docs.len()
        ))),
    }
}

fn ng_stream<T: Serialize>(docs: &[T]) -> Result<String, Failure> {
    let mut out = Vec::new();
    let mut ser = serde_yaml_ng::Serializer::new(&mut out);
    for d in docs {
        d.serialize(&mut ser).map_err(rejected)?;
    }
    ser.into_inner().map_err(rejected)?;
    String::from_utf8(out).map_err(rejected)
}

fn norway_stream<T: Serialize>(docs: &[T]) -> Result<String, Failure> {
    let mut out = Vec::new();
    let mut ser = serde_norway::Serializer::new(&mut out);
    for d in docs {
        d.serialize(&mut ser).map_err(rejected)?;
    }
    ser.into_inner().map_err(rejected)?;
    String::from_utf8(out).map_err(rejected)
}

/// Writes `docs` as one YAML stream (for [`Lib::Json`], JSON: the array of
/// the documents when `opts.multi`, else the one document), with the
/// library's defaults. Without `opts.multi`, `docs` must hold exactly one
/// document. `opts.merge` plays no part: the data is already merged.
///
/// # Errors
/// [`Failure::Unsupported`] when the library has no multi-document writer,
/// [`Failure::Rejected`] when it reports an error.
pub fn write<T: Serialize>(lib: Lib, docs: &[T], opts: Opts) -> Result<String, Failure> {
    if let Some(why) = write_unsupported(lib, opts) {
        return Err(Failure::Unsupported(why));
    }
    let multi = opts.multi;
    match lib {
        Lib::YamlNg if multi => ng_stream(docs),
        Lib::YamlNg => serde_yaml_ng::to_string(one(docs)?).map_err(rejected),
        Lib::Norway if multi => norway_stream(docs),
        Lib::Norway => serde_norway::to_string(one(docs)?).map_err(rejected),
        Lib::Yaml2 => serde_yaml2::to_string(one(docs)?).map_err(rejected),
        Lib::Saphyr if multi => serde_saphyr::to_string_multiple(docs).map_err(rejected),
        Lib::Saphyr => serde_saphyr::to_string(one(docs)?).map_err(rejected),
        Lib::Json if multi => serde_json::to_string(docs).map_err(rejected),
        Lib::Json => serde_json::to_string(one(docs)?).map_err(rejected),
    }
}

/// [`write()`] for a [`Dyn`] read by [`value`] (the write benchmark's
/// `value` workload: each library writes its own dynamic value).
///
/// # Errors
/// As [`write()`].
pub fn write_dyn(lib: Lib, value: &Dyn, opts: Opts) -> Result<String, Failure> {
    match value {
        Dyn::YamlNg(v) => write(lib, v, opts),
        Dyn::Norway(v) => write(lib, v, opts),
        Dyn::Yaml2(v) => write(lib, v, opts),
        Dyn::Json(v) => write(lib, v, opts),
    }
}

/// Reads every document of `input` into the library's dynamic value type.
///
/// # Errors
/// As [`typed`].
pub fn value(lib: Lib, input: &str, opts: Opts) -> Result<Dyn, Failure> {
    if let Some(why) = unsupported(lib, opts) {
        return Err(Failure::Unsupported(why));
    }
    match lib {
        Lib::YamlNg => ng_values(input, opts).map(Dyn::YamlNg),
        Lib::Norway => norway_values(input, opts).map(Dyn::Norway),
        Lib::Yaml2 => typed(lib, input, opts).map(Dyn::Yaml2),
        Lib::Saphyr | Lib::Json => typed(lib, input, opts).map(Dyn::Json),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use serde_json::json;

    const MULTI: Opts = Opts {
        multi: true,
        merge: false,
    };
    const MERGE: Opts = Opts {
        multi: false,
        merge: true,
    };

    #[test]
    fn names_round_trip() {
        for l in Lib::ALL {
            assert_eq!(Lib::from_name(l.name()), Some(*l));
        }
        assert_eq!(Lib::from_name("nope"), None);
        assert!(!Lib::YAML.contains(&Lib::Json));
    }

    #[test]
    fn every_library_reads_values() {
        for l in Lib::ALL {
            let input = if *l == Lib::Json {
                r#"{"a": [1, "x"]}"#
            } else {
                "a: [1, x]\n"
            };
            let v = value(*l, input, Opts::default()).unwrap();
            assert_eq!(v.to_json().unwrap(), [json!({"a": [1, "x"]})], "{l:?}");
            assert!(!v.is_empty());
        }
    }

    #[test]
    fn multi_documents_and_merges() {
        let multi = "a: 1\n---\na: 2\n";
        let merge = "b: &b {x: 1}\nc:\n  <<: *b\n  y: 2\n";
        let want = json!({"b": {"x": 1}, "c": {"x": 1, "y": 2}});
        for l in [Lib::YamlNg, Lib::Norway, Lib::Saphyr] {
            let v = value(l, multi, MULTI).unwrap();
            assert_eq!(v.to_json().unwrap(), [json!({"a": 1}), json!({"a": 2})]);
            let t: Vec<serde_json::Value> = typed(l, multi, MULTI).unwrap();
            assert_eq!(t.len(), 2);
            let v = value(l, merge, MERGE).unwrap();
            assert_eq!(v.to_json().unwrap(), std::slice::from_ref(&want), "{l:?}");
            let t: Vec<serde_json::Value> = typed(l, merge, MERGE).unwrap();
            assert_eq!(t, std::slice::from_ref(&want), "{l:?}");
        }
        let j: Vec<serde_json::Value> = typed(Lib::Json, "[1, 2]", MULTI).unwrap();
        assert_eq!(j, [json!(1), json!(2)]);
    }

    #[test]
    fn feature_complete_comparators() {
        let gated: Vec<Lib> = Lib::ALL
            .iter()
            .copied()
            .filter(|l| l.feature_complete())
            .collect();
        assert_eq!(gated, [Lib::YamlNg, Lib::Norway, Lib::Saphyr]);
    }

    /// Every library writes its own value of a document so that it reads
    /// back as the same data, one document and several.
    #[test]
    fn every_library_writes() {
        let yaml = "a: [1, x, true, null, 2.5]\nb: {c: \"it's\"}\n";
        let json_in = r#"{"a": [1, "x", true, null, 2.5], "b": {"c": "it's"}}"#;
        let want = json!({"a": [1, "x", true, null, 2.5], "b": {"c": "it's"}});
        for l in Lib::ALL {
            let input = if *l == Lib::Json { json_in } else { yaml };
            let d = value(*l, input, Opts::default()).unwrap();
            let out = write_dyn(*l, &d, Opts::default()).unwrap();
            if *l == Lib::Json {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&out).unwrap(),
                    want
                );
                continue;
            }
            let back = value(Lib::YamlNg, &out, Opts::default()).map(|v| v.to_json());
            if *l == Lib::Yaml2 {
                // serde_yaml2 writes `'it\'s'`: a backslash does not
                // escape inside single quotes, so no reader gets `it's`.
                assert_ne!(back.ok(), Some(Ok(vec![want.clone()])), "{out}");
                continue;
            }
            assert_eq!(
                back.unwrap().unwrap(),
                std::slice::from_ref(&want),
                "{l:?}: {out}"
            );
        }
        let multi = "a: 1\n---\na: 2\n";
        for l in [Lib::YamlNg, Lib::Norway, Lib::Saphyr] {
            let d = value(l, multi, MULTI).unwrap();
            let out = write_dyn(l, &d, MULTI).unwrap();
            let back = value(Lib::YamlNg, &out, MULTI).unwrap();
            assert_eq!(
                back.to_json().unwrap(),
                [json!({"a": 1}), json!({"a": 2})],
                "{l:?}: {out}"
            );
        }
        let d = value(Lib::Json, "[{\"a\": 1}, {\"a\": 2}]", MULTI).unwrap();
        assert_eq!(
            write_dyn(Lib::Json, &d, MULTI).unwrap(),
            r#"[{"a":1},{"a":2}]"#
        );
        assert!(matches!(
            write(Lib::Yaml2, &[1, 2], MULTI),
            Err(Failure::Unsupported(_))
        ));
        assert!(matches!(
            write(Lib::YamlNg, &[1, 2], Opts::default()),
            Err(Failure::Rejected(_))
        ));
    }

    #[test]
    fn unsupported_and_rejected() {
        assert!(matches!(
            value(Lib::Yaml2, "a: 1\n", MULTI),
            Err(Failure::Unsupported(_))
        ));
        assert!(matches!(
            typed::<serde_json::Value>(Lib::Yaml2, "a: 1\n", MERGE),
            Err(Failure::Unsupported(_))
        ));
        for l in Lib::ALL {
            assert!(
                matches!(
                    value(*l, "[a, b\n", Opts::default()),
                    Err(Failure::Rejected(_))
                ),
                "{l:?}"
            );
        }
    }
}
