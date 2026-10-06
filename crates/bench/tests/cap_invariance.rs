//! Meaning never depends on
//! `max_diagnostics`. For every input of the suite (three settings), the
//! benchmark corpus (cases up to 256 KB), the adversarial corpus
//! (`untrusted()`), and inputs past the cap of every kind (decoding,
//! syntax errors and warnings, meaning errors and warnings), every
//! meaning-level outcome is identical under `max_diagnostics` ∈ {0, 1, 2,
//! 16, the profile's, none}:
//!
//! * reads: `from_str`/`from_slice` into a `Value`, strict `Document` and
//!   `Stream` parses, each document's `to_value` (a value, or the error's
//!   code, span, and path: past the cap an error's *message* is its code's
//!   summary, which is all the cap may change);
//! * `has_errors` of the stream and of each document, and each error
//!   region's code and span;
//! * edit acceptance (restricted mode included) of a sample script per
//!   document, the edited text, and `has_errors` after it.
//!
//! Only the stored diagnostic lists may differ.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fmt::Write as _;

use oneyaml::{Compat, Document, Error, Limits, NodeKind, Settings, Stream, Value};

fn err(e: &Error) -> String {
    format!(
        "Err({} {:?} {:?})",
        e.code(),
        e.span(),
        e.path().map(ToString::to_string)
    )
}

fn res<T: std::fmt::Debug>(r: &Result<T, Error>) -> String {
    match r {
        Ok(v) => format!("Ok({v:?})"),
        Err(e) => err(e),
    }
}

/// The first word of an edit error's `Debug` form: its kind.
fn kind(e: &oneyaml::EditError) -> String {
    let s = format!("{e:?}");
    s.split(['(', ' ', '{']).next().unwrap_or("").to_owned()
}

fn stream_outcome(out: &mut String, st: &Stream) {
    let _ = writeln!(out, "stream has_errors {}", st.has_errors());
    for (i, d) in st.documents().enumerate().take(12) {
        let regions: Vec<_> = d
            .errors()
            .map(|r| r.diagnostic().map(|x| (x.code, x.primary.span)))
            .collect();
        let _ = writeln!(
            out,
            "doc{i} has_errors {} value {} regions {regions:?}",
            d.has_errors(),
            res(&d.to_value())
        );
        // The sample script, each step on a copy of the stream: an
        // insertion, removing the first and a middle entry, and replacing
        // the first entry's value (edits that move, drop, and
        // re-read the diagnostics around them).
        let items: Vec<oneyaml::NodeId> = match d.root() {
            Some(r) if r.kind() == NodeKind::Mapping => r
                .as_mapping()
                .unwrap()
                .iter()
                .map(|(_, v)| v.id())
                .collect(),
            Some(r) if r.kind() == NodeKind::Sequence => {
                r.as_sequence().unwrap().iter().map(|v| v.id()).collect()
            }
            _ => Vec::new(),
        };
        for step in 0..4 {
            edit_step(out, st, i, step, &items);
        }
    }
}

/// One step of the sample script on a copy of `st`, with its outcome.
fn edit_step(out: &mut String, st: &Stream, i: usize, step: usize, items: &[oneyaml::NodeId]) {
    {
        let mut copy = st.clone();
        let mut dm = copy.document_mut(i).unwrap();
        let r = match (step, items.len()) {
            (0, _) => match dm.root().map(|n| n.kind()) {
                Some(NodeKind::Mapping) => dm
                    .root_mut()
                    .and_then(|mut m| m.insert("zz_cap", 1).map(|n| n.id()))
                    .map(|_| ()),
                Some(NodeKind::Sequence) => dm.root_mut().and_then(|mut s| s.push(1).map(|_| ())),
                _ => dm.set_root(1).map(|_| ()),
            },
            (_, 0) => return,
            (1, _) => dm.remove_node(items[0]).map(|_| ()),
            (2, n) => dm.remove_node(items[n / 2]).map(|_| ()),
            _ => dm
                .node_mut(items[0])
                .and_then(|mut n| n.set("zz").map(|_| ())),
        };
        let _ = write!(out, "  step {step} ");
        match r {
            Ok(()) => {
                let after = copy.document(i).unwrap();
                let _ = writeln!(
                    out,
                    "  edit ok {:?} has_errors {} value {}",
                    copy.to_string(),
                    after.has_errors(),
                    res(&after.to_value())
                );
            }
            Err(e) => {
                let _ = writeln!(out, "  edit {}", kind(&e));
            }
        }
    }
}

/// Every meaning-level outcome of `src` under `s`.
pub(crate) fn outcome(src: &str, s: &Settings) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "from_str {}",
        res(&oneyaml::from_str_with::<Value>(src, s))
    );
    let _ = writeln!(
        out,
        "Document::parse {}",
        res(&Document::parse_with(src, s).map(|d| d.to_string()))
    );
    match Stream::parse_with(src, s) {
        Ok(st) => {
            out.push_str("Stream::parse ok\n");
            stream_outcome(&mut out, &st);
        }
        Err(e) => {
            let _ = writeln!(out, "Stream::parse {}", err(&e));
        }
    }
    let p = Stream::parse_recovering_with(src, s);
    out.push_str("recovering\n");
    stream_outcome(&mut out, p.value());
    let d = Document::parse_recovering_with(src, s);
    let _ = writeln!(
        out,
        "document has_errors {} value {}",
        d.value().has_errors(),
        res(&d.value().to_value())
    );
    out
}

fn bytes_outcome(bytes: &[u8], s: &Settings) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "from_slice {}",
        res(&oneyaml::from_slice_with::<Value>(bytes, s))
    );
    let p = Stream::from_slice_recovering(bytes, s);
    stream_outcome(&mut out, p.value());
    let d = Document::from_slice_recovering(bytes, s);
    let _ = writeln!(
        out,
        "document has_errors {} value {}",
        d.value().has_errors(),
        res(&d.value().to_value())
    );
    out
}

fn limits_of(s: &Settings) -> Vec<Settings> {
    let base = s.get_limits().clone();
    let mut all: Vec<Limits> = [0usize, 1, 2, 16]
        .into_iter()
        .map(|n| base.clone().max_diagnostics(n))
        .collect();
    all.push(base.clone());
    all.push(base.max_diagnostics(usize::MAX));
    all.into_iter().map(|l| s.clone().limits(l)).collect()
}

pub(crate) fn same(label: &str, s: &Settings, f: &dyn Fn(&Settings) -> String) {
    let all = limits_of(s);
    let want = f(all.last().unwrap());
    for t in &all[..all.len() - 1] {
        let got = f(t);
        assert!(
            got == want,
            "{label}: max_diagnostics {} changes a meaning-level outcome\n--- with it\n{got}\n--- without\n{want}",
            t.get_limits().get_max_diagnostics()
        );
    }
}

/// Inputs past the cap of every kind, small enough for every limit.
fn past_the_cap() -> Vec<(String, String, Settings)> {
    let lenient = Settings::new().lenient_inline_colons(true);
    let mut v = vec![
        (
            "orphans".to_owned(),
            "%YAML 1.2\n...\n".repeat(20),
            Settings::new(),
        ),
        (
            "unclosed".to_owned(),
            "a: [\n---\n".repeat(20),
            Settings::new(),
        ),
        (
            "tags".to_owned(),
            "--- !!int x\n".repeat(20),
            Settings::new(),
        ),
        (
            "dups".to_owned(),
            format!("a: 0\n{}", "a: 1\n".repeat(20)),
            Compat::serde_yaml(),
        ),
        (
            "dups-then-error".to_owned(),
            format!("{}b: !!int x\n", "a: 1\n".repeat(20)),
            Compat::serde_yaml(),
        ),
        (
            "colons".to_owned(),
            "- a: b: c\n".repeat(20),
            lenient.clone(),
        ),
        (
            "colons-then-error".to_owned(),
            format!("{}- [\n", "- a: b: c\n".repeat(20)),
            lenient.clone(),
        ),
        (
            "colons-docs".to_owned(),
            "--- \n- a: b: c\n- a: b: c\n".repeat(10),
            lenient,
        ),
        (
            "mixed".to_owned(),
            "--- !!int x\n--- {a: 1, a: 2}\n--- [\n".repeat(8),
            Compat::serde_yaml(),
        ),
    ];
    v.push((
        "dup-errors".to_owned(),
        "--- {a: 1, a: 2}\n".repeat(20),
        Settings::new(),
    ));
    v
}

#[test]
fn meaning_never_depends_on_max_diagnostics() {
    let suite = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    assert!(suite.len() >= 402);
    let settings = [
        ("default", Settings::new()),
        ("serde_yaml", Compat::serde_yaml()),
        ("lenient", Settings::new().lenient_inline_colons(true)),
    ];
    let mut items = 0usize;
    for (name, s) in &settings {
        for c in &suite {
            let src = String::from_utf8_lossy(&c.input).into_owned();
            same(&format!("suite/{name}/{}", c.id), s, &|t| outcome(&src, t));
            items += 1;
        }
    }
    for (name, src, s) in past_the_cap() {
        same(&format!("past/{name}"), &s, &|t| outcome(&src, t));
        items += 1;
    }
    for (name, bytes) in [
        ("invalid-bytes", b"--- \xff\n".repeat(20)),
        ("invalid-runs", b"a: \x80b\x80c\x80\n".repeat(10)),
    ] {
        same(&format!("bytes/{name}"), &Settings::new(), &|t| {
            bytes_outcome(&bytes, t)
        });
        items += 1;
    }
    for a in test_suite::adversarial::corpus() {
        let s = Settings::new()
            .limits(Limits::untrusted())
            .merge_keys(a.merge_keys);
        same(&format!("adversarial/{}", a.name), &s, &|t| {
            outcome(&a.input, t)
        });
        items += 1;
    }
    for case in bench::corpus::CASES.iter().filter(|c| c.base <= 256 * 1024) {
        let src = case.generate(1);
        same(&format!("bench/{}", case.name), &Settings::new(), &|t| {
            outcome(&src, t)
        });
        items += 1;
    }
    eprintln!("{items} inputs, 6 limits each");
}
