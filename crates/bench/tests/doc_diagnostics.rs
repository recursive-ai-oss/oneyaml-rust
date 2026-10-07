//! A guard: the diagnostics of the strict and
//! recovering `Document`/`Stream` entry points must not change while their
//! meaning check is made cheaper.
//!
//! For every input below, under each reading setting, the outcome of
//! `Document::parse_with`, `Stream::parse_with`,
//! `Document::parse_recovering_with` and `Stream::parse_recovering_with`
//! is written out in full — every diagnostic's `Debug` form (code,
//! severity, message, primary and secondary labels with their spans, help,
//! path, fixes), each stream document's own diagnostics, and for an error
//! its diagnostic, path, warnings and `Display` — and each group of inputs
//! hashes to what the same file recorded before that work
//! (`crates/bench/data/doc-diagnostics-baseline.tsv`). `SOURCE` says how.
//!
//! The inputs:
//!
//! 1. every yaml-test-suite case, and every truncation of it at a
//!    character boundary;
//! 2. hand-written meaning cases (duplicate keys in small and large
//!    mappings, merge keys, aliases, tags, schemas, directives, warnings);
//! 3. deterministic mutations of the suite and of (2);
//! 4. the benchmark corpus at 1× (cases up to 1 MB);
//! 5. the adversarial corpus under `Limits::untrusted()`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fmt::Write as _;

use oneyaml::{
    Compat, Diagnostic, Document, DuplicateKeys, Error, Limits, Schema, Settings, Stream,
};

/// How the baseline was recorded:
///
/// ```sh
/// REV=37508b2   # the revision before the cheaper meaning check
/// git archive $REV -- . | tar -x -C /tmp/$REV   # from this workspace
/// cp crates/bench/tests/doc_diagnostics.rs /tmp/$REV/crates/bench/tests/
/// ONEYAML_DOC_BASELINE_WRITE=$PWD/crates/bench/data/doc-diagnostics-baseline.tsv \
///     cargo test --release -p bench --test doc_diagnostics
/// ```
///
/// (run in `/tmp/$REV`). `ONEYAML_DOC_DUMP=<file>` writes every item's
/// outcome instead of hashes, to diff two revisions item by item.
///
/// The adversarial corpus later gained two `Y2009`
/// cases, so the `adversarial/untrusted` line alone was re-recorded at the
/// tree of that change (this file run with `ONEYAML_DOC_BASELINE_WRITE` to a
/// scratch file, that one line copied in; every other line was
/// byte-identical and kept). A `#` line in the file says so.
///
/// The groups holding a reworded `Y0005`, `Y0007`, `Y0011` or `Y0017`
/// message (whose one-line form now names the likely cause) were
/// re-recorded the same way, in both files, after an item-by-item
/// `ONEYAML_DOC_DUMP` comparison with the previous release showed that only
/// those diagnostics changed; a `#` line says so. The code is written in
/// the recorded `Code(5)` form (`recorded`).
const SOURCE: &str = "crates/bench/data/doc-diagnostics-baseline.tsv";

/// The groups of mutations that contain a byte order mark (U+FEFF). Treating a
/// mark where no document prefix may start as a new error (`Y0032`, §5.2)
/// changed them, so these are compared with `BOM_SOURCE`, recorded after that
/// change, not with `SOURCE`. Every other group is still compared with
/// `SOURCE`. Recorded by running this file with
/// `ONEYAML_DOC_BOM_BASELINE_WRITE=$PWD/crates/bench/data/doc-diagnostics-bom-baseline.tsv`,
/// last after the related changes (accepting a mark that starts a document
/// prefix after a complete document, and reporting marks inside comments and
/// block scalars; labelling the mark that ended a document, reading what
/// follows as the next document, and reporting a mark before the error it
/// causes; emitting that report after the error when its label comes later);
/// `SOURCE` was re-recorded as its doc comment says, with this file (the split
/// only moved mutations containing a mark out of `mutations/*`). Item by item,
/// the new outcomes differed from the old ones exactly for the 330 items whose
/// outcome had `Y0032` (all in `mutations/*`).
const BOM_SOURCE: &str = "crates/bench/data/doc-diagnostics-bom-baseline.tsv";

/// The name prefix of the byte order mark groups (see `BOM_SOURCE`).
const BOM_GROUP: &str = "mutations-bom/";

/// FNV-1a 64.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
}

fn settings() -> Vec<(&'static str, Settings)> {
    vec![
        ("default", Settings::new()),
        ("merge_keys", Settings::new().merge_keys(true)),
        (
            "last_wins",
            Settings::new()
                .duplicate_keys(DuplicateKeys::LastWins)
                .merge_keys(true),
        ),
        ("json", Settings::new().schema(Schema::Json)),
        ("failsafe", Settings::new().schema(Schema::Failsafe)),
        ("yaml_1_1", Compat::yaml_1_1()),
        ("serde_yaml", Compat::serde_yaml()),
        (
            "honor_lenient",
            Settings::new()
                .honor_version_directive(true)
                .lenient_inline_colons(true),
        ),
    ]
}

/// `d`'s `Debug` form with the code written as the recording wrote it
/// (`Code(5)`), whatever `Code`'s own `Debug` prints.
fn recorded(d: &Diagnostic) -> String {
    format!("{d:?}").replacen(
        &format!("code: {}", d.code),
        &format!("code: Code({})", d.code.get()),
        1,
    )
}

fn diags(out: &mut String, what: &str, list: &[Diagnostic]) {
    for d in list {
        let _ = writeln!(out, "  {what} {}", recorded(d));
    }
}

fn error(out: &mut String, e: &Error) {
    let _ = writeln!(out, "  err {e}");
    let _ = writeln!(out, "  path {:?}", e.path().map(ToString::to_string));
    diags(out, "diag", std::slice::from_ref(e.diagnostic()));
    diags(out, "warn", e.warnings());
}

/// Every entry point's outcome on `src` under `s`, in full.
fn outcome(src: &str, s: &Settings) -> String {
    let mut out = String::new();
    out.push_str("Document::parse\n");
    match Document::parse_with(src, s) {
        Ok(d) => diags(&mut out, "ok", &d.diagnostics()),
        Err(e) => error(&mut out, &e),
    }
    out.push_str("Stream::parse\n");
    match Stream::parse_with(src, s) {
        Ok(st) => {
            diags(&mut out, "ok", &st.diagnostics());
            for (i, d) in st.documents().enumerate() {
                diags(&mut out, &format!("doc{i}"), &d.diagnostics());
            }
        }
        Err(e) => error(&mut out, &e),
    }
    out.push_str("Document::parse_recovering\n");
    let p = Document::parse_recovering_with(src, s);
    diags(&mut out, "parsed", p.diagnostics());
    diags(&mut out, "doc", &p.value().diagnostics());
    out.push_str("Stream::parse_recovering\n");
    let p = Stream::parse_recovering_with(src, s);
    diags(&mut out, "parsed", p.diagnostics());
    diags(&mut out, "stream", &p.value().diagnostics());
    for (i, d) in p.value().documents().enumerate() {
        diags(&mut out, &format!("doc{i}"), &d.diagnostics());
    }
    out
}

/// Meaning cases the suite has few of (the check's shortcuts).
fn extra() -> Vec<String> {
    let mut v: Vec<String> = [
        "a: 1\na: 2\n",
        "a: 1\nb: 2\na: 3\n",
        "1: x\n0x1: y\n",
        "1: x\n01: y\n",
        "1.0: x\n1: y\n",
        "1.0: x\n1.00: y\n",
        "true: x\nTrue: y\n",
        "null: x\n~: y\n'': z\n",
        "'a': 1\n\"a\": 2\n",
        "a: 1\n'a': 2\n",
        "? [a, b]\n: 1\n? [a, b]\n: 2\n",
        "? {a: 1, b: 2}\n: x\n? {b: 2, a: 1}\n: y\n",
        "{a: 1, a: 2}\n",
        "[{a: 1, a: 2}]\n",
        "- a: 1\n  a: 2\n",
        "&x a: 1\n*x : 2\n",
        "a: &x [1]\nb: *x\nc: *y\n",
        "a: &x [1, *x]\n",
        "base: &b {a: 1}\nd:\n  <<: *b\n  a: 2\n",
        "d:\n  <<: 1\n",
        "d:\n  <<: [1]\n",
        "d:\n  <<: {a: 1}\n  <<: {b: 2}\n",
        "d:\n  !!str <<: x\n",
        "d:\n  '<<': x\n  '<<': y\n",
        "&m <<: {a: 1}\nx: {*m : {b: 2}}\n",
        "a: !!int x\n",
        "a: !!float x\n",
        "a: !!bool yes\n",
        "a: !!null x\n",
        "a: !!map x\n",
        "a: !!seq [1]\nb: !!map [1]\n",
        "a: !!str 1\nb: !!int '2'\n",
        "a: !custom x\nb: !<tag:x,2000:y> [1]\n",
        "%TAG !e! tag:example.com,2000:\n--- !e!x a\n",
        "hello\n",
        "{a: hello}\n",
        "[1, true, null, 1.5]\n",
        "%YAML 1.1\n--- a: yes\n",
        "%YAML 1.3\n--- a\n",
        "%YAML 2.0\n--- a\n",
        "a\n--- b\n--- c: 1\n  c: 2\n",
        "a: 1\n--- {b: 1, b: 2}\n--- *x\n",
        "title: The farm: a banana story\n",
        "a: b: c\n",
        "a: 08\nb: 0o17\nc: 0b101\nd: 1_000\ne: 1:30\n",
        "a: [1, 2\nb: {c: 1, c: 2}\n",
        "{a: 1, a: 2, [b: 3\n",
        "- !!int x\n- [\n",
        "a: 1\na: 2\nb: [\n",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    // Mappings around the small-key bound: a duplicate at every position,
    // in block and flow, with plain, quoted and numeric keys.
    for n in [2usize, 7, 8, 9, 10, 16, 17, 30] {
        for dup in [0usize, n / 2, n.saturating_sub(1)] {
            let mut block = String::new();
            let mut flow = String::from("{");
            for i in 0..n {
                let _ = writeln!(block, "k{i}: {i}");
                let _ = write!(flow, "{i}: v, ");
            }
            let _ = writeln!(block, "k{dup}: again");
            let _ = writeln!(flow, "0x{dup:x}: again}}");
            v.push(block.clone());
            v.push(flow);
            v.push(format!("'q': 1\n{block}\"q\": 2\n"));
            v.push(format!("top:\n{}", block.replace('k', "  k")));
        }
    }
    v
}

/// A small deterministic generator (xorshift64*), self-contained so the
/// baseline revision generates the same inputs.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % n.max(1)
    }
}

const ALPHABET: &[&str] = &[
    "-", "?", ":", ": ", ",", "[", "]", "{", "}", "#", "&a ", "*a", "!", "!!int ", "!!str ",
    "!!map ", "|", ">", "'", "\"", "%", "@", " ", "  ", "\n", "\t", "\r\n", "---", "...", "<<",
    "<<: ", "x", "k1: ", "1", "0x1", "\\", "\u{feff}", "é", "~", "null", ".inf", "08",
];

fn mutate(rng: &mut Rng, src: &str) -> String {
    let mut s = src.to_owned();
    for _ in 0..=rng.below(3) {
        let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        let at = bounds[rng.below(bounds.len())];
        let end = bounds.iter().copied().find(|&b| b > at).unwrap_or(at);
        match rng.below(3) {
            0 => s.insert_str(at, ALPHABET[rng.below(ALPHABET.len())]),
            1 => s.replace_range(at..end, ""),
            _ => s.replace_range(at..end, ALPHABET[rng.below(ALPHABET.len())]),
        }
    }
    s
}

/// A named group of `(label, input, settings)` items.
type Group = (String, Vec<(String, String, Settings)>);

/// Every group as `(name, items)`, in a fixed order.
fn groups() -> Vec<Group> {
    let suite = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    assert!(suite.len() >= 402, "{}", suite.len());
    let texts: Vec<(String, String)> = suite
        .iter()
        .map(|c| (c.id.clone(), String::from_utf8_lossy(&c.input).into_owned()))
        .collect();
    let extra = extra();
    let mut out = Vec::new();
    for (label, s) in settings() {
        let mut g = Vec::new();
        for (id, src) in &texts {
            g.push((id.clone(), src.clone(), s.clone()));
        }
        out.push((format!("suite/{label}"), g));
        let mut g = Vec::new();
        for (id, src) in &texts {
            for (cut, _) in src.char_indices().skip(1) {
                g.push((format!("{id}@{cut}"), src[..cut].to_owned(), s.clone()));
            }
        }
        out.push((format!("truncations/{label}"), g));
        let g = extra
            .iter()
            .enumerate()
            .map(|(i, src)| (format!("extra{i}"), src.clone(), s.clone()))
            .collect();
        out.push((format!("extra/{label}"), g));
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ label.len() as u64);
        let mut g = Vec::new();
        for round in 0..3000 {
            let base = if round % 3 == 0 {
                &extra[rng.below(extra.len())]
            } else {
                &texts[rng.below(texts.len())].1
            };
            g.push((format!("m{round}"), mutate(&mut rng, base), s.clone()));
        }
        // What a byte order mark inside a document means changed
        // (a new error), so mutations containing a mark are their own
        // group, recorded after that change (`BOM_SOURCE`).
        let (bom, plain): (Vec<_>, Vec<_>) = g
            .into_iter()
            .partition(|(_, src, _)| src.contains('\u{feff}'));
        out.push((format!("mutations/{label}"), plain));
        out.push((format!("{BOM_GROUP}{label}"), bom));
    }
    // the baselines were recorded before the corpus gained
    // helm-values, so it is not part of this comparison.
    for case in bench::corpus::CASES
        .iter()
        .filter(|c| c.base <= 1024 * 1024 && c.name != bench::corpus::HELM_VALUES)
    {
        let src = case.generate(1);
        for (label, s) in settings()
            .into_iter()
            .filter(|(l, _)| matches!(*l, "default" | "merge_keys" | "yaml_1_1"))
        {
            out.push((
                format!("bench/{}/{label}", case.name),
                vec![(case.name.to_owned(), src.clone(), s)],
            ));
        }
    }
    let g = test_suite::adversarial::corpus()
        .into_iter()
        .map(|a| {
            let s = Settings::new()
                .limits(Limits::untrusted())
                .merge_keys(a.merge_keys);
            (a.name.to_owned(), a.input, s)
        })
        .collect();
    out.push(("adversarial/untrusted".to_owned(), g));
    out
}

#[test]
fn diagnostics_match_the_recorded_baseline() {
    let dump = std::env::var("ONEYAML_DOC_DUMP").ok();
    let mut dump_text = String::new();
    let mut lines = Vec::new();
    let mut items = 0usize;
    for (name, group) in groups() {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for (label, src, s) in &group {
            let o = outcome(src, s);
            fnv(&mut h, label.as_bytes());
            fnv(&mut h, b"\0");
            fnv(&mut h, o.as_bytes());
            fnv(&mut h, b"\x1e");
            if dump.is_some() {
                let _ = writeln!(dump_text, "== {name} {label}\n{o}");
            }
        }
        items += group.len();
        lines.push(format!("{name}\t{}\t{h:016x}", group.len()));
    }
    if let Some(path) = dump {
        std::fs::write(path, dump_text).unwrap();
    }
    let (bom_lines, lines): (Vec<String>, Vec<String>) =
        lines.into_iter().partition(|l| l.starts_with(BOM_GROUP));
    let mut written = false;
    for (var, lines) in [
        ("ONEYAML_DOC_BASELINE_WRITE", &lines),
        ("ONEYAML_DOC_BOM_BASELINE_WRITE", &bom_lines),
    ] {
        if let Ok(path) = std::env::var(var) {
            let mut text = String::from(
                "# group\titems\tfnv1a64 of labels and outcomes (see doc_diagnostics.rs)\n",
            );
            for l in lines {
                text.push_str(l);
                text.push('\n');
            }
            std::fs::write(&path, text).unwrap();
            written = true;
        }
    }
    if written {
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap();
    for (source, lines) in [(SOURCE, &lines), (BOM_SOURCE, &bom_lines)] {
        let recorded =
            std::fs::read_to_string(root.join(source)).unwrap_or_else(|e| panic!("{source}: {e}"));
        let recorded: Vec<&str> = recorded.lines().filter(|l| !l.starts_with('#')).collect();
        let differing: Vec<String> = lines
            .iter()
            .zip(recorded.iter())
            .filter(|(now, then)| now.as_str() != **then)
            .map(|(now, then)| format!("now  {now}\nthen {then}"))
            .collect();
        assert!(
            differing.is_empty() && lines.len() == recorded.len(),
            "{} of {} groups differ from {source} ({} vs {} groups):\n{}",
            differing.len(),
            lines.len(),
            lines.len(),
            recorded.len(),
            differing.join("\n")
        );
    }
    let lines = lines.len().saturating_add(bom_lines.len());
    eprintln!("{lines} groups, {items} inputs, 4 entry points each");
}
