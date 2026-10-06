//! The serde path against YAML serde comparators (serde ≤ 1.25× the
//! fastest comparator), on the real-world corpus.
//!
//! Three workloads, each run by every library on the same input:
//!
//! * **value**: every document into the library's own dynamic value type
//!   (`oneyaml::Value`, `serde_yaml_ng::Value`, …; see
//!   [`oracles::serde_cmp`] for the ones without one);
//! * **typed**: every document into the realistic derived structs of
//!   [`crate::models`];
//! * **sparse**: every document into a struct reading a few fields.
//!
//! [`measured_drain_serde`] is the measured function (its name matches the
//! `*measured_drain*` callgrind toggle, so the probe plumbing is shared with
//! the parser tables). [`check`] is the correctness guard run before any
//! measurement: a comparator whose result differs from ours, or that can't
//! do the equivalent work, is not measured.

use std::hint::black_box;
use std::path::PathBuf;

use oneyaml::Settings;
use oracles::serde_cmp::{self, Dyn, Failure, Lib, Opts};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::corpus::{self, Case, Kind};
use crate::models::{compose, gha, helm, k8s, openapi};

/// A serde implementation under measurement: ours or a comparator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SerdeLib {
    /// `oneyaml::from_str_with` / `oneyaml::Deserializer::from_str_with`.
    OneYaml,
    /// A comparator from `oracles`.
    Other(Lib),
}

impl SerdeLib {
    /// Every column, ours first, then the YAML comparators, then serde_json.
    pub const ALL: &'static [SerdeLib] = &[
        SerdeLib::OneYaml,
        SerdeLib::Other(Lib::YamlNg),
        SerdeLib::Other(Lib::Norway),
        SerdeLib::Other(Lib::Yaml2),
        SerdeLib::Other(Lib::Saphyr),
        SerdeLib::Other(Lib::Json),
    ];

    /// Stable command-line / table name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SerdeLib::OneYaml => "oneyaml",
            SerdeLib::Other(l) => l.name(),
        }
    }

    /// Looks a library up by [`SerdeLib::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<SerdeLib> {
        SerdeLib::ALL.iter().copied().find(|l| l.name() == name)
    }

    /// Reads JSON rather than YAML.
    #[must_use]
    pub fn is_json(self) -> bool {
        self == SerdeLib::Other(Lib::Json)
    }
}

/// What is deserialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Workload {
    /// Reads the input and does nothing else (subtracted from DHAT figures).
    Baseline,
    /// The library's dynamic value type.
    Value,
    /// The realistic derived structs.
    Typed,
    /// A struct reading a few fields.
    Sparse,
}

impl Workload {
    /// The measured workloads, in table order.
    pub const MEASURED: &'static [Workload] = &[Workload::Value, Workload::Typed, Workload::Sparse];

    /// Stable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Workload::Baseline => "baseline",
            Workload::Value => "value",
            Workload::Typed => "typed",
            Workload::Sparse => "sparse",
        }
    }

    /// Looks a workload up by [`Workload::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Workload> {
        [Workload::Baseline]
            .iter()
            .chain(Workload::MEASURED)
            .copied()
            .find(|w| w.name() == name)
    }
}

/// Which model family a real-world case uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    K8s,
    Gha,
    Compose,
    OpenApi,
    Helm,
}

pub(crate) fn shape(case: &Case) -> Option<Shape> {
    match case.name {
        "k8s-manifest" | "k8s-multi" => Some(Shape::K8s),
        "gha-workflow" => Some(Shape::Gha),
        "compose-merge" => Some(Shape::Compose),
        "openapi-100k" | "openapi-10m" => Some(Shape::OpenApi),
        corpus::HELM_VALUES => Some(Shape::Helm),
        _ => None,
    }
}

/// The serde cases: every real-world corpus input, at 1×.
#[must_use]
pub fn cases() -> Vec<&'static Case> {
    corpus::CASES
        .iter()
        .filter(|c| c.kind == Kind::RealWorld && shape(c).is_some())
        .collect()
}

/// What equivalent work needs on `case`: every document of the k8s
/// streams, and merge keys on compose-merge.
#[must_use]
pub fn opts(case: &Case) -> Opts {
    let s = shape(case);
    Opts {
        multi: s == Some(Shape::K8s),
        merge: s == Some(Shape::Compose),
    }
}

/// Our settings for `case`: the defaults (including `Limits::default()`),
/// plus `merge_keys` where the input relies on `<<` (off by default).
#[must_use]
pub const fn settings(opts: Opts) -> Settings {
    Settings::new().merge_keys(opts.merge)
}

pub(crate) fn ours<T: DeserializeOwned>(input: &str, opts: Opts) -> Result<Vec<T>, Failure> {
    let s = settings(opts);
    let err = |e: oneyaml::Error| Failure::Rejected(e.to_string());
    if opts.multi {
        oneyaml::Deserializer::from_str_with(input, &s)
            .map(|doc| T::deserialize(doc).map_err(err))
            .collect()
    } else {
        oneyaml::from_str_with(input, &s)
            .map(|t| vec![t])
            .map_err(err)
    }
}

/// One run's result: the document count, and the documents as JSON when
/// asked for (the guard), never inside a measurement.
struct Out {
    docs: usize,
    json: Option<Vec<serde_json::Value>>,
}

fn to_json<T: Serialize>(docs: &[T]) -> Result<Vec<serde_json::Value>, Failure> {
    docs.iter()
        .map(|d| serde_json::to_value(d).map_err(|e| Failure::Rejected(format!("to JSON: {e}"))))
        .collect()
}

fn typed<T: DeserializeOwned + Serialize>(
    lib: SerdeLib,
    input: &str,
    opts: Opts,
    keep: bool,
) -> Result<Out, Failure> {
    let docs: Vec<T> = match lib {
        SerdeLib::OneYaml => ours(input, opts)?,
        SerdeLib::Other(l) => serde_cmp::typed(l, input, opts)?,
    };
    let docs = black_box(docs);
    let json = if keep { Some(to_json(&docs)?) } else { None };
    Ok(Out {
        docs: docs.len(),
        json,
    })
}

fn value(lib: SerdeLib, input: &str, opts: Opts, keep: bool) -> Result<Out, Failure> {
    match lib {
        SerdeLib::OneYaml => typed::<oneyaml::Value>(lib, input, opts, keep),
        SerdeLib::Other(l) => {
            let v: Dyn = black_box(serde_cmp::value(l, input, opts)?);
            let json = if keep {
                Some(v.to_json().map_err(Failure::Rejected)?)
            } else {
                None
            };
            Ok(Out {
                docs: v.len(),
                json,
            })
        }
    }
}

fn run(
    lib: SerdeLib,
    workload: Workload,
    case: &Case,
    input: &str,
    keep: bool,
) -> Result<Out, Failure> {
    let o = opts(case);
    let Some(s) = shape(case) else {
        return Err(Failure::Unsupported("not a serde case"));
    };
    match (workload, s) {
        (Workload::Baseline, _) => Ok(Out {
            docs: input.len(),
            json: None,
        }),
        (Workload::Value, _) => value(lib, input, o, keep),
        (Workload::Typed, Shape::K8s) => typed::<k8s::Object>(lib, input, o, keep),
        (Workload::Typed, Shape::Gha) => typed::<gha::Workflow>(lib, input, o, keep),
        (Workload::Typed, Shape::Compose) => typed::<compose::Compose>(lib, input, o, keep),
        (Workload::Typed, Shape::OpenApi) => typed::<openapi::OpenApi>(lib, input, o, keep),
        (Workload::Typed, Shape::Helm) => typed::<helm::Values>(lib, input, o, keep),
        (Workload::Sparse, Shape::K8s) => typed::<k8s::Sparse>(lib, input, o, keep),
        (Workload::Sparse, Shape::Gha) => typed::<gha::Sparse>(lib, input, o, keep),
        (Workload::Sparse, Shape::Compose) => typed::<compose::Sparse>(lib, input, o, keep),
        (Workload::Sparse, Shape::OpenApi) => typed::<openapi::Sparse>(lib, input, o, keep),
        (Workload::Sparse, Shape::Helm) => typed::<helm::Sparse>(lib, input, o, keep),
    }
}

/// Deserializes `input` (from [`probe_input`]) with `lib` and drops the
/// result; returns the document count.
///
/// `#[inline(never)]` and a name matching `*measured_drain*`: callgrind
/// counts only while this is on the stack.
///
/// # Errors
/// The library rejected the input, or can't do the equivalent work.
#[inline(never)]
pub fn measured_drain_serde(
    lib: SerdeLib,
    workload: Workload,
    case: &Case,
    input: &str,
) -> Result<usize, Failure> {
    let input = black_box(input);
    black_box(run(lib, workload, case, input, false).map(|o| o.docs))
}

/// The input `lib` reads for `case`: the generated YAML, or for serde_json
/// the equivalent JSON ([`json_input`]).
///
/// # Errors
/// As [`json_input`].
pub fn probe_input(lib: SerdeLib, case: &Case) -> Result<String, String> {
    if lib.is_json() {
        json_input(case)
    } else {
        Ok(case.generate(1))
    }
}

/// The JSON equivalent of `case`: our `Value` of each document (merge keys
/// expanded), as one document or an array of the documents. It is cached
/// in a file keyed by the YAML's FNV-1a, so a probe **reads** its input
/// (like the baseline) instead of paying for the conversion in its heap
/// peak.
///
/// # Errors
/// If we reject the case, or the file can't be written or read.
pub fn json_input(case: &Case) -> Result<String, String> {
    let yaml = case.generate(1);
    let dir = std::env::temp_dir().join("oneyaml-bench-json");
    let path: PathBuf = dir.join(format!(
        "{}-{:016x}.json",
        case.name,
        corpus::fnv1a(yaml.as_bytes())
    ));
    if let Ok(text) = std::fs::read_to_string(&path) {
        return Ok(text);
    }
    let o = opts(case);
    let docs: Vec<oneyaml::Value> = ours(&yaml, o).map_err(|e| format!("{}: {e:?}", case.name))?;
    let json = if o.multi {
        serde_json::to_string(&docs)
    } else {
        serde_json::to_string(docs.first().unwrap_or(&oneyaml::Value::Null))
    }
    .map_err(|e| format!("{}: to JSON: {e}", case.name))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Write then rename, so a concurrent reader never sees a partial file.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, &json).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(json)
}

/// The correctness guard's verdict for one library, workload and case.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// Same documents as ours.
    Agrees,
    /// The library returned something else; where it first differs.
    Differs(String),
    /// The library can't do the equivalent work; why.
    Unsupported(String),
    /// The library rejected the input; its message.
    Rejects(String),
    /// We rejected the input (never expected on the corpus).
    OursRejects(String),
}

impl Verdict {
    /// One line: a keyword, a space, the detail.
    #[must_use]
    pub fn line(&self) -> String {
        let one = |s: &str| s.replace(['\n', '|'], " ");
        match self {
            Verdict::Agrees => "agrees".to_owned(),
            Verdict::Differs(d) => format!("differs {}", one(d)),
            Verdict::Unsupported(d) => format!("unsupported {}", one(d)),
            Verdict::Rejects(d) => format!("rejects {}", one(d)),
            Verdict::OursRejects(d) => format!("ours-rejects {}", one(d)),
        }
    }

    /// Parses [`Verdict::line`].
    #[must_use]
    pub fn parse(line: &str) -> Option<Verdict> {
        let line = line.trim();
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.to_owned();
        Some(match word {
            "agrees" => Verdict::Agrees,
            "differs" => Verdict::Differs(rest),
            "unsupported" => Verdict::Unsupported(rest),
            "rejects" => Verdict::Rejects(rest),
            "ours-rejects" => Verdict::OursRejects(rest),
            _ => return None,
        })
    }
}

/// Runs ours and `lib` on `case` and compares the documents as
/// `serde_json::Value`s.
#[must_use]
pub fn check(lib: SerdeLib, workload: Workload, case: &Case) -> Verdict {
    let yaml = case.generate(1);
    let want = match run(SerdeLib::OneYaml, workload, case, &yaml, true) {
        Ok(Out { json: Some(j), .. }) => j,
        Ok(Out { json: None, .. }) => return Verdict::Agrees,
        Err(Failure::Rejected(e)) => return Verdict::OursRejects(e),
        Err(Failure::Unsupported(e)) => return Verdict::OursRejects(e.to_owned()),
        Err(_) => return Verdict::OursRejects("unknown failure".to_owned()),
    };
    let input = match probe_input(lib, case) {
        Ok(i) => i,
        Err(e) => return Verdict::OursRejects(e),
    };
    let got = match run(lib, workload, case, &input, true) {
        Ok(Out { json: Some(j), .. }) => j,
        Ok(Out { json: None, .. }) => return Verdict::Agrees,
        Err(Failure::Rejected(e)) => return Verdict::Rejects(truncate(&e)),
        Err(Failure::Unsupported(e)) => return Verdict::Unsupported(e.to_owned()),
        Err(_) => return Verdict::Rejects("unknown failure".to_owned()),
    };
    match first_diff(&want, &got) {
        None => Verdict::Agrees,
        Some(d) => Verdict::Differs(truncate(&d)),
    }
}

pub(crate) fn truncate(s: &str) -> String {
    let mut out: String = s.chars().take(160).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

fn short(v: &serde_json::Value) -> String {
    truncate(&v.to_string())
}

/// Where two document lists first differ, as `doc N` plus a JSON pointer.
/// Iterative (explicit stack), so any nesting depth is fine.
#[must_use]
pub fn first_diff(want: &[serde_json::Value], got: &[serde_json::Value]) -> Option<String> {
    use serde_json::Value as J;
    if want.len() != got.len() {
        return Some(format!("{} documents, ours {}", got.len(), want.len()));
    }
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        let mut stack = vec![(String::new(), w, g)];
        while let Some((path, w, g)) = stack.pop() {
            match (w, g) {
                (J::Object(a), J::Object(b)) => {
                    for (k, av) in a.iter().rev() {
                        let Some(bv) = b.get(k) else {
                            return Some(format!(
                                "doc {i} {path}/{k}: missing (ours {})",
                                short(av)
                            ));
                        };
                        stack.push((format!("{path}/{k}"), av, bv));
                    }
                    if let Some(k) = b.keys().find(|k| !a.contains_key(*k)) {
                        return Some(format!("doc {i} {path}/{k}: extra key, not in ours"));
                    }
                }
                (J::Array(a), J::Array(b)) if a.len() == b.len() => {
                    for (j, (av, bv)) in a.iter().zip(b).enumerate().rev() {
                        stack.push((format!("{path}/{j}"), av, bv));
                    }
                }
                _ if w == g => {}
                _ => {
                    return Some(format!("doc {i} {path}: {} (ours {})", short(g), short(w)));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use serde_json::json;

    #[test]
    fn names_round_trip() {
        for l in SerdeLib::ALL {
            assert_eq!(SerdeLib::from_name(l.name()), Some(*l));
        }
        for w in Workload::MEASURED {
            assert_eq!(Workload::from_name(w.name()), Some(*w));
        }
        assert_eq!(Workload::from_name("baseline"), Some(Workload::Baseline));
        assert_eq!(SerdeLib::from_name("x"), None);
        assert_eq!(Workload::from_name("x"), None);
    }

    #[test]
    fn every_real_world_case_is_a_serde_case() {
        let n = corpus::CASES
            .iter()
            .filter(|c| c.kind == Kind::RealWorld)
            .count();
        assert_eq!(cases().len(), n);
        assert!(opts(corpus::find("k8s-multi").unwrap()).multi);
        assert!(opts(corpus::find("compose-merge").unwrap()).merge);
        assert_eq!(opts(corpus::find("gha-workflow").unwrap()), Opts::default());
    }

    #[test]
    fn diffs_name_the_first_difference() {
        let a = [json!({"a": [1, {"b": 2}], "c": 3})];
        assert_eq!(first_diff(&a, &a), None);
        let b = [json!({"a": [1, {"b": 5}], "c": 3})];
        assert_eq!(first_diff(&a, &b).unwrap(), "doc 0 /a/1/b: 5 (ours 2)");
        let c = [json!({"a": [1, {"b": 2}]})];
        assert!(first_diff(&a, &c).unwrap().contains("/c: missing"));
        assert!(first_diff(&c, &a).unwrap().contains("extra key"));
        assert!(first_diff(&a, &[]).unwrap().contains("documents"));
    }

    #[test]
    fn verdict_lines_round_trip() {
        for v in [
            Verdict::Agrees,
            Verdict::Differs("doc 0 /a: 1".into()),
            Verdict::Unsupported("no".into()),
            Verdict::Rejects("bad".into()),
            Verdict::OursRejects("bad".into()),
        ] {
            assert_eq!(Verdict::parse(&v.line()), Some(v));
        }
        assert_eq!(Verdict::parse("what"), None);
    }

    /// Ours reads every case in every workload, with the same document
    /// count as the JSON reference, and the guard agrees with itself.
    #[test]
    fn ours_reads_the_small_cases() {
        for c in cases().into_iter().filter(|c| c.base <= 1024 * 1024) {
            let yaml = c.generate(1);
            for w in Workload::MEASURED {
                let n = measured_drain_serde(SerdeLib::OneYaml, *w, c, &yaml).unwrap();
                assert!(n >= 1, "{} {}", c.name, w.name());
                assert_eq!(check(SerdeLib::OneYaml, *w, c), Verdict::Agrees);
                let lib = SerdeLib::Other(Lib::Json);
                let json = probe_input(lib, c).unwrap();
                let theirs = measured_drain_serde(lib, *w, c, &json);
                match check(lib, *w, c) {
                    Verdict::Agrees => assert_eq!(theirs.unwrap(), n),
                    // JSON has no source text: `RUST_BACKTRACE: 1` is a
                    // number there, which a `String` field rejects.
                    Verdict::Rejects(e) => {
                        assert_eq!((c.name, *w), ("gha-workflow", Workload::Typed), "{e}");
                        assert!(theirs.is_err());
                    }
                    other => {
                        assert!(
                            matches!(other, Verdict::Agrees | Verdict::Rejects(_)),
                            "{} {}: {other:?}",
                            c.name,
                            w.name()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn merge_keys_are_expanded_for_compose() {
        let c = corpus::find("compose-merge").unwrap();
        let v: Vec<compose::Compose> = ours(&c.generate(1), opts(c)).unwrap();
        let db = &v[0].services["db"];
        assert_eq!(db.restart.as_deref(), Some("unless-stopped"));
        assert_eq!(db.networks, ["backend"]);
    }
}
