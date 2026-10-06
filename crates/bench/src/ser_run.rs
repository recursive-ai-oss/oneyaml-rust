//! The write direction of the serde path against the YAML serde
//! comparators (serde paths ≤ 1.25× the fastest feature-complete
//! comparator), on the real-world corpus.
//!
//! Two workloads, each writing every document of a case as one YAML stream
//! (serde_json: one JSON text):
//!
//! * **value**: the library's own dynamic value, read beforehand by that
//!   library from the case (serde_json: from the equivalent JSON). Ours is
//!   `Value::to_yaml_string` per document (the no-clone path; documents
//!   after the first are preceded by `---`, as `Serializer` writes them);
//! * **typed**: the realistic derived structs of [`crate::models`], read
//!   beforehand by ours, the same data for every library. Ours is
//!   `oneyaml::to_string` for one document and one `oneyaml::Serializer`
//!   (a `serialize` per document) for a stream.
//!
//! Preparing the data is outside the measured function
//! ([`measured_drain_ser`], whose name matches the `*measured_drain*`
//! callgrind toggle); the DHAT baseline of every cell prepares the same data
//! and writes nothing. [`check`] is the correctness guard of the write
//! direction: the output is read back with *our* reader (serde_json's with
//! serde_json) and must give the same documents as the data written, so a
//! library that silently writes something else (a quoted scalar read back
//! as a number, a lost document, a broken escape) is never measured.

use std::hint::black_box;

use oracles::serde_cmp::{self, Dyn, Failure, Lib, Opts};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::corpus::Case;
use crate::models::{compose, gha, helm, k8s, openapi};
use crate::serde_run::{SerdeLib, Shape, Verdict, first_diff, ours, probe_input, shape, truncate};

/// What is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SerWorkload {
    /// The library's own dynamic value type.
    Value,
    /// The realistic derived structs.
    Typed,
}

impl SerWorkload {
    /// The measured workloads, in table order.
    pub const MEASURED: &'static [SerWorkload] = &[SerWorkload::Value, SerWorkload::Typed];

    /// Stable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SerWorkload::Value => "value",
            SerWorkload::Typed => "typed",
        }
    }

    /// Looks a workload up by [`SerWorkload::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<SerWorkload> {
        SerWorkload::MEASURED
            .iter()
            .copied()
            .find(|w| w.name() == name)
    }
}

/// One write probe: `ser:<lib>:<workload>`, or with `:baseline` the same
/// preparation without the write (subtracted from the DHAT figures).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerProbe {
    /// The writer.
    pub lib: SerdeLib,
    /// What it writes.
    pub workload: SerWorkload,
    /// Prepare the data only.
    pub baseline: bool,
}

impl SerProbe {
    /// The probe name.
    #[must_use]
    pub fn name(self) -> String {
        let base = if self.baseline { ":baseline" } else { "" };
        format!("ser:{}:{}{base}", self.lib.name(), self.workload.name())
    }

    /// Parses [`SerProbe::name`].
    #[must_use]
    pub fn parse(name: &str) -> Option<SerProbe> {
        let rest = name.strip_prefix("ser:")?;
        let (lib, rest) = rest.split_once(':')?;
        let (workload, baseline) = match rest.split_once(':') {
            Some((w, "baseline")) => (w, true),
            Some(_) => return None,
            None => (rest, false),
        };
        Some(SerProbe {
            lib: SerdeLib::from_name(lib)?,
            workload: SerWorkload::from_name(workload)?,
            baseline,
        })
    }
}

/// The documents a library writes.
#[derive(Debug)]
#[non_exhaustive]
pub enum Data {
    /// Our `Value`s.
    Ours(Vec<oneyaml::Value>),
    /// A comparator's own dynamic values.
    Dyn(Dyn),
    /// Kubernetes objects.
    K8s(Vec<k8s::Object>),
    /// GitHub Actions workflows.
    Gha(Vec<gha::Workflow>),
    /// Compose files.
    Compose(Vec<compose::Compose>),
    /// OpenAPI documents.
    OpenApi(Vec<openapi::OpenApi>),
    /// Helm values files.
    Helm(Vec<helm::Values>),
}

impl Data {
    /// Number of documents.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Data::Ours(v) => v.len(),
            Data::Dyn(v) => v.len(),
            Data::K8s(v) => v.len(),
            Data::Gha(v) => v.len(),
            Data::Compose(v) => v.len(),
            Data::OpenApi(v) => v.len(),
            Data::Helm(v) => v.len(),
        }
    }

    /// No documents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What writing `case` needs: every document of a stream. Merge keys play
/// no part (the data is already merged).
#[must_use]
pub fn opts(case: &Case) -> Opts {
    Opts {
        multi: crate::serde_run::opts(case).multi,
        merge: false,
    }
}

/// Reads the data `lib` writes for `workload` on `case`.
///
/// # Errors
/// The reader rejected the case, or `lib` can't do the equivalent work
/// (no multi-document writer).
pub fn prepare(lib: SerdeLib, workload: SerWorkload, case: &Case) -> Result<Data, Failure> {
    let read_opts = crate::serde_run::opts(case);
    if let SerdeLib::Other(l) = lib
        && let Some(why) = serde_cmp::write_unsupported(l, opts(case))
    {
        return Err(Failure::Unsupported(why));
    }
    let Some(s) = shape(case) else {
        return Err(Failure::Unsupported("not a serde case"));
    };
    let yaml = || case.generate(1);
    match (workload, lib) {
        (SerWorkload::Value, SerdeLib::OneYaml) => ours(&yaml(), read_opts).map(Data::Ours),
        (SerWorkload::Value, SerdeLib::Other(l)) => {
            let input = probe_input(lib, case).map_err(Failure::Rejected)?;
            serde_cmp::value(l, &input, read_opts).map(Data::Dyn)
        }
        (SerWorkload::Typed, _) => match s {
            Shape::K8s => ours(&yaml(), read_opts).map(Data::K8s),
            Shape::Gha => ours(&yaml(), read_opts).map(Data::Gha),
            Shape::Compose => ours(&yaml(), read_opts).map(Data::Compose),
            Shape::OpenApi => ours(&yaml(), read_opts).map(Data::OpenApi),
            Shape::Helm => ours(&yaml(), read_opts).map(Data::Helm),
        },
    }
}

fn err(e: oneyaml::Error) -> Failure {
    Failure::Rejected(e.to_string())
}

/// Ours, typed: `to_string` for one document, one `Serializer` for several.
fn ours_typed<T: Serialize>(docs: &[T], opts: Opts) -> Result<String, Failure> {
    if !opts.multi {
        let [doc] = docs else {
            return Err(Failure::Rejected("not one document".to_owned()));
        };
        return oneyaml::to_string(doc).map_err(err);
    }
    let mut out = Vec::new();
    let mut ser = oneyaml::Serializer::new(&mut out);
    for d in docs {
        d.serialize(&mut ser).map_err(err)?;
    }
    ser.into_inner().map_err(err)?;
    String::from_utf8(out).map_err(|e| Failure::Rejected(e.to_string()))
}

/// Ours, value: `Value::to_yaml_string` per document, `---` between them.
fn ours_value(docs: &[oneyaml::Value]) -> Result<String, Failure> {
    let mut out = String::new();
    for (i, d) in docs.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        out.push_str(&d.to_yaml_string().map_err(err)?);
    }
    Ok(out)
}

fn typed_with<T: Serialize>(lib: SerdeLib, docs: &[T], opts: Opts) -> Result<String, Failure> {
    match lib {
        SerdeLib::OneYaml => ours_typed(docs, opts),
        SerdeLib::Other(l) => serde_cmp::write(l, docs, opts),
    }
}

/// Writes `data` with `lib` (not measured; see [`measured_drain_ser`]).
///
/// # Errors
/// The writer failed, or `data` is not what `lib` writes.
pub fn write(lib: SerdeLib, data: &Data, opts: Opts) -> Result<String, Failure> {
    match (lib, data) {
        (SerdeLib::OneYaml, Data::Ours(v)) => ours_value(v),
        (SerdeLib::Other(l), Data::Dyn(v)) => serde_cmp::write_dyn(l, v, opts),
        (_, Data::K8s(v)) => typed_with(lib, v, opts),
        (_, Data::Gha(v)) => typed_with(lib, v, opts),
        (_, Data::Compose(v)) => typed_with(lib, v, opts),
        (_, Data::OpenApi(v)) => typed_with(lib, v, opts),
        (_, Data::Helm(v)) => typed_with(lib, v, opts),
        _ => Err(Failure::Unsupported("data prepared for another library")),
    }
}

/// Writes `data` with `lib` and drops the output; returns its length.
///
/// `#[inline(never)]` and a name matching `*measured_drain*`: callgrind
/// counts only while this is on the stack.
///
/// # Errors
/// As [`write()`].
#[inline(never)]
pub fn measured_drain_ser(lib: SerdeLib, data: &Data, opts: Opts) -> Result<usize, Failure> {
    let data = black_box(data);
    black_box(write(lib, data, opts).map(|out| black_box(out).len()))
}

fn json_of<T: Serialize>(docs: &[T]) -> Result<Vec<serde_json::Value>, String> {
    docs.iter()
        .map(|d| serde_json::to_value(d).map_err(|e| format!("to JSON: {e}")))
        .collect()
}

/// Reads `out` back as `T` documents: ours, or serde_json for serde_json.
fn read_back<T: DeserializeOwned>(lib: SerdeLib, out: &str, opts: Opts) -> Result<Vec<T>, Failure> {
    if lib.is_json() {
        serde_cmp::typed(Lib::Json, out, opts)
    } else {
        ours(out, opts)
    }
}

/// Typed: the documents read back equal the documents written.
fn same_typed<T: DeserializeOwned + Serialize + PartialEq>(
    lib: SerdeLib,
    docs: &[T],
    out: &str,
    opts: Opts,
) -> Verdict {
    let back: Vec<T> = match read_back(lib, out, opts) {
        Ok(b) => b,
        Err(e) => return unreadable(&e),
    };
    if back.as_slice() == docs {
        return Verdict::Agrees;
    }
    match (json_of(docs), json_of(&back)) {
        (Ok(want), Ok(got)) => Verdict::Differs(truncate(
            &first_diff(&want, &got).unwrap_or_else(|| "differs as Rust values".to_owned()),
        )),
        (Err(e), _) | (_, Err(e)) => Verdict::Differs(e),
    }
}

fn unreadable(e: &Failure) -> Verdict {
    match e {
        Failure::Rejected(m) => {
            Verdict::Differs(truncate(&format!("output does not read back: {m}")))
        }
        Failure::Unsupported(m) => Verdict::Differs((*m).to_owned()),
        _ => Verdict::Differs("output does not read back".to_owned()),
    }
}

/// The correctness guard of the write direction, plus the output length
/// when there is one: prepares `lib`'s data, writes it, reads the output
/// back and compares it with the documents the case holds.
#[must_use]
pub fn check(lib: SerdeLib, workload: SerWorkload, case: &Case) -> (Verdict, Option<usize>) {
    let o = opts(case);
    let data = match prepare(lib, workload, case) {
        Ok(d) => d,
        Err(Failure::Unsupported(e)) => return (Verdict::Unsupported(e.to_owned()), None),
        Err(Failure::Rejected(e)) if lib == SerdeLib::OneYaml => {
            return (Verdict::OursRejects(e), None);
        }
        Err(Failure::Rejected(e)) => {
            return (Verdict::Rejects(truncate(&format!("reading: {e}"))), None);
        }
        Err(_) => return (Verdict::Rejects("unknown failure".to_owned()), None),
    };
    let out = match write(lib, &data, o) {
        Ok(out) => out,
        Err(Failure::Unsupported(e)) => return (Verdict::Unsupported(e.to_owned()), None),
        Err(Failure::Rejected(e)) if lib == SerdeLib::OneYaml => {
            return (Verdict::OursRejects(e), None);
        }
        Err(Failure::Rejected(e)) => return (Verdict::Rejects(truncate(&e)), None),
        Err(_) => return (Verdict::Rejects("unknown failure".to_owned()), None),
    };
    let verdict = match &data {
        Data::K8s(d) => same_typed(lib, d, &out, o),
        Data::Gha(d) => same_typed(lib, d, &out, o),
        Data::Compose(d) => same_typed(lib, d, &out, o),
        Data::OpenApi(d) => same_typed(lib, d, &out, o),
        Data::Helm(d) => same_typed(lib, d, &out, o),
        Data::Ours(_) | Data::Dyn(_) => same_value(lib, case, &out, o),
    };
    (verdict, Some(out.len()))
}

/// Value: the output read back (as JSON) equals our reading of the case.
fn same_value(lib: SerdeLib, case: &Case, out: &str, opts: Opts) -> Verdict {
    let want: Vec<oneyaml::Value> = match ours(&case.generate(1), crate::serde_run::opts(case)) {
        Ok(w) => w,
        Err(Failure::Rejected(e)) => return Verdict::OursRejects(e),
        Err(_) => return Verdict::OursRejects("unknown failure".to_owned()),
    };
    let got: Result<Vec<serde_json::Value>, String> = if lib.is_json() {
        match serde_cmp::typed(Lib::Json, out, opts) {
            Ok(g) => Ok(g),
            Err(e) => return unreadable(&e),
        }
    } else {
        match ours::<oneyaml::Value>(out, opts) {
            Ok(g) => json_of(&g),
            Err(e) => return unreadable(&e),
        }
    };
    match (json_of(&want), got) {
        (Ok(w), Ok(g)) => {
            first_diff(&w, &g).map_or(Verdict::Agrees, |d| Verdict::Differs(truncate(&d)))
        }
        (Err(e), _) | (_, Err(e)) => Verdict::Differs(e),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use crate::corpus;
    use crate::serde_run::cases;

    #[test]
    fn probe_names_round_trip() {
        for l in SerdeLib::ALL {
            for w in SerWorkload::MEASURED {
                for baseline in [false, true] {
                    let p = SerProbe {
                        lib: *l,
                        workload: *w,
                        baseline,
                    };
                    assert_eq!(SerProbe::parse(&p.name()), Some(p));
                }
            }
        }
        assert_eq!(SerProbe::parse("serde:oneyaml:value"), None);
        assert_eq!(SerProbe::parse("ser:oneyaml:value:x"), None);
        assert_eq!(SerProbe::parse("ser:nope:value"), None);
        assert_eq!(SerProbe::parse("ser:oneyaml:sparse"), None);
        assert_eq!(SerWorkload::from_name("typed"), Some(SerWorkload::Typed));
    }

    /// Ours writes every small case in both workloads so that it reads
    /// back as what was written, and the measured function agrees with
    /// the checked one on the output length.
    #[test]
    fn ours_writes_the_small_cases() {
        for c in cases().into_iter().filter(|c| c.base <= 1024 * 1024) {
            for w in SerWorkload::MEASURED {
                let (v, len) = check(SerdeLib::OneYaml, *w, c);
                assert_eq!(v, Verdict::Agrees, "{} {}", c.name, w.name());
                let data = prepare(SerdeLib::OneYaml, *w, c).unwrap();
                assert!(!data.is_empty());
                let n = measured_drain_ser(SerdeLib::OneYaml, &data, opts(c)).unwrap();
                assert_eq!(Some(n), len);
            }
        }
    }

    /// The guard measures the feature-complete comparators on a stream
    /// and refuses serde_yaml2 there (no multi-document writer).
    #[test]
    fn comparators_on_a_stream() {
        let c = corpus::find("k8s-manifest").unwrap();
        for l in [Lib::YamlNg, Lib::Norway, Lib::Saphyr, Lib::Json] {
            for w in SerWorkload::MEASURED {
                let (v, len) = check(SerdeLib::Other(l), *w, c);
                assert_eq!(v, Verdict::Agrees, "{} {}", l.name(), w.name());
                assert!(len.unwrap() > 1000);
            }
        }
        let (v, len) = check(SerdeLib::Other(Lib::Yaml2), SerWorkload::Typed, c);
        assert!(matches!(v, Verdict::Unsupported(_)), "{v:?}");
        assert_eq!(len, None);
        assert!(matches!(
            prepare(SerdeLib::Other(Lib::Yaml2), SerWorkload::Value, c),
            Err(Failure::Unsupported(_))
        ));
    }

    /// Data prepared for one library is refused by another's writer, and
    /// output that reads back differently is a `Differs` verdict.
    #[test]
    fn mismatches_are_reported() {
        let c = corpus::find("gha-workflow").unwrap();
        let data = prepare(SerdeLib::OneYaml, SerWorkload::Value, c).unwrap();
        assert!(matches!(
            write(SerdeLib::Other(Lib::YamlNg), &data, opts(c)),
            Err(Failure::Unsupported(_))
        ));
        let v = same_value(SerdeLib::OneYaml, c, "a: 1\n", opts(c));
        assert!(matches!(v, Verdict::Differs(_)), "{v:?}");
        let v = same_value(SerdeLib::OneYaml, c, "[a\n", opts(c));
        assert!(
            matches!(&v, Verdict::Differs(d) if d.contains("does not read back")),
            "{v:?}"
        );
        let data = prepare(SerdeLib::OneYaml, SerWorkload::Typed, c).unwrap();
        let Data::Gha(docs) = data else {
            assert!(matches!(data, Data::Gha(_)), "gha data: {data:?}");
            return;
        };
        let mut other = docs.clone();
        other[0].name = "changed".to_owned();
        let out = ours_typed(&other, opts(c)).unwrap();
        let v = same_typed(SerdeLib::OneYaml, &docs, &out, opts(c));
        assert!(
            matches!(&v, Verdict::Differs(d) if d.contains("/name")),
            "{v:?}"
        );
    }
}
