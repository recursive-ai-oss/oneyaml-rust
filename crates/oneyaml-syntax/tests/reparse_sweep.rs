//! An edited tree always agrees with its own
//! text, recovered input included): an edit sweep in which every accepted
//! edit is compared, whole ([`compare_trees`]: structure, token texts,
//! block headers, tags, and every diagnostic with its labels and fixes),
//! with a fresh parse of the edited text under the tree's own settings.
//!
//! Inputs: the yaml-test-suite, every fuzz corpus and the fuzz
//! regressions (recovered inputs included), the suite and consistency
//! inputs with their comment lines amplified (stray `...` among them), and
//! shapes found while hardening the edit layer: the inputs found then,
//! recovered flow collections cut by a document marker, lenient values in
//! explicit keys, a byte order mark after white space, tabs, and flow keys
//! longer than the 1024-character implicit-key window. Edits at sampled
//! elements of each (all of them for the reported inputs): removal; white
//! space, tabs, line breaks, comment lines, a scalar and a sequence entry
//! inserted before and after (line-start inserts included); comments
//! rewritten; white space shortened, lengthened and swapped with tabs; line
//! breaks rewritten (`\r\n` included); scalars rewritten (a lenient `a: b`
//! included); nodes replaced. Both under the default settings and under
//! `lenient_inline_colons`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use std::fs;
use std::path::{Path, PathBuf};

use oneyaml_core::Settings;
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxKind as K, SyntaxTree, compare_trees};

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Every file under `dir` (recursively) named `name` (any, for `None`),
/// sorted, with its path relative to `crates/`.
fn files(dir: &Path, name: Option<&str>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries {
            let p = e.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if name.is_none_or(|n| p.file_name().is_some_and(|f| f == n)) {
                let bytes = fs::read(&p).unwrap();
                let rel = p
                    .strip_prefix(crates())
                    .map_or_else(|_| p.display().to_string(), |r| r.display().to_string());
                out.push((rel, String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
    }
    out.sort();
    out
}

/// `src` with every comment line followed by up to `k` copies of it at
/// nearby columns, blank lines, and stray `...` markers (an
/// amplification).
fn amplify(src: &str, rng: &mut Rng, k: usize) -> String {
    let mut out = String::new();
    for line in src.split_inclusive('\n') {
        out.push_str(line);
        let t = line.trim_start_matches([' ', '\t']);
        if !t.starts_with('#') {
            continue;
        }
        let col = line.len() - t.len();
        let body = t.trim_end_matches(['\n', '\r']);
        if !line.ends_with('\n') {
            out.push('\n');
        }
        for i in 0..k {
            match rng.below(14) {
                0 => out.push('\n'),
                1 => out.push_str("...\n"),
                _ => {
                    let c = (col + rng.below(7)).saturating_sub(3);
                    out.push_str(&" ".repeat(c));
                    out.push_str(body);
                    out.push_str(&format!(" {i}\n"));
                }
            }
        }
    }
    out
}

/// The shapes found while hardening the edit layer, and recovered and
/// long-key shapes around them.
fn synthetic() -> Vec<(String, String)> {
    let long = "k".repeat(1100);
    let mut v: Vec<(String, String)> = [
        ("lenient value in an explicit key", "? : a: b\n".to_owned()),
        (
            "lenient value, nested",
            "- ? : x: y\n  : z\n- k: a: b\n".to_owned(),
        ),
        ("unclosed flow cut by ...", "[ a\n# c\n...\n".to_owned()),
        (
            "unclosed flow, then a document",
            "a: [b\n# c\n--- \nd: e\n".to_owned(),
        ),
        (
            "unclosed flow, then an entry",
            "a: [b\n# c\nd: e\n".to_owned(),
        ),
        ("unclosed quote", "k: \"x\n# c\n  y\n".to_owned()),
        (
            "unclosed flow in a sequence",
            "- [a, b\n  # c\n- c\n".to_owned(),
        ),
        (
            "unclosed flow, nested",
            "a:\n  b: {c: d\n  # x\ne: f\n".to_owned(),
        ),
        (
            "error after clean entries",
            "a: 1 # c\nb:\n  - x\nc: [d\n".to_owned(),
        ),
        (
            "error after a block scalar",
            "a: |\n  t\n\nb: ]\n".to_owned(),
        ),
        ("long flow key", format!("{{{long}: v, b: c}}\n")),
        ("long flow pair", format!("[{long}: v]\n")),
        (
            "long flow key in a sequence",
            format!("- {{{long} : v}}\n- x\n"),
        ),
        ("long entry, unclosed", format!("a: [{long}, b\n")),
        ("long block key", format!("{long}: v\nb: c\n")),
        (
            "block scalar key before trailing spaces",
            "? |\n  ".to_owned(),
        ),
        ("block scalar key, nested", "a:\n  ? |\n   \n".to_owned()),
        ("folded key in a sequence", "- ? >\n    \n- x\n".to_owned()),
        (
            "block scalar key, then a document",
            "? |-\n   \n--- y\n".to_owned(),
        ),
        ("multi-line flow key", "[a,\n b]: c\nd: e\n".to_owned()),
        (
            "multi-line flow key in flow",
            "{x: [a,\n b]: c}\nd: e\n".to_owned(),
        ),
        ("multi-line plain key", "a\nb: c\nd: e\n".to_owned()),
        ("colon in a continuation", "k: a\n  b: c\nd: e\n".to_owned()),
        (
            "multi-line key in a sequence",
            "- [a,\n  b]: c\n- d\n".to_owned(),
        ),
        ("multi-line quoted key", "\"a\n b\": c\nd: e\n".to_owned()),
        (
            "multi-line flow key, unclosed",
            "[[a, x\n ab]: 1]\n".to_owned(),
        ),
        (
            "multi-line flow key, nested",
            "[[a, [b,\n c], d]: 1]\n".to_owned(),
        ),
        ("long flow key, unclosed", format!("[[a, {long}]: 1]\n")),
        (
            "stray bracket after a flow mapping",
            "{a: 1, b: 2} ]\n".to_owned(),
        ),
        (
            "stray bracket after a value",
            "k: {a: 1, b: 2} ]\nz: 1\n".to_owned(),
        ),
        (
            "error after a clean mapping",
            "good:\n  x: 1\nbad: @x\nmore: [1]\n".to_owned(),
        ),
        ("tab indentation", "a:\n  b: 1\n\tc: 2\n".to_owned()),
        ("tabs after indicators", "- a\n -\tb\n- c: [d\n".to_owned()),
        ("byte order marks", "\u{feff}a: 1\n  \u{feff}b\n".to_owned()),
        (
            "white space before a mark",
            " \u{feff}- a\n - b\n".to_owned(),
        ),
        (
            "a mark ending a document",
            "a: 1\n\u{feff}b\n---\nc: d\n".to_owned(),
        ),
        (
            "directive error, then content",
            "%YAML 2.0\n---\na: 1 # c\nb: [x]\n".to_owned(),
        ),
        (
            "recovered documents among clean ones",
            "a: 1\n---\nb: [c\n...\nd: e\n---\n- f\n".to_owned(),
        ),
        // Unclosed quotes before a byte order mark at the start
        // of a line (it ends the document, production 211).
        (
            "unclosed quote, then a mark and a marker",
            "'a\n\u{feff}---\n".to_owned(),
        ),
        (
            "unclosed quote, then a mark line",
            "'x\n\u{feff}\na\n".to_owned(),
        ),
        (
            "unclosed double quote, then a mark line",
            "\"x\n\u{feff}\na\n".to_owned(),
        ),
        (
            "unclosed quote in a value, then a mark",
            "k: 'x\n\u{feff}b: c\n---\nd\n".to_owned(),
        ),
        (
            "unclosed quote in a sequence, then a mark",
            "- \"x\n  y\n\u{feff}\n...\nz\n".to_owned(),
        ),
        (
            "unclosed quote in flow, then a mark",
            "[ 'x\n\u{feff}--- y\n".to_owned(),
        ),
        (
            "closed quote across a mark line",
            "'x\n\u{feff}y'\n# c\n".to_owned(),
        ),
        (
            "unclosed quote, a mark, a comment",
            "'a\n\u{feff}# c\n---\nb\n".to_owned(),
        ),
    ]
    .into_iter()
    .map(|(n, s)| (format!("synthetic: {n}"), s))
    .collect();
    // CRLF variants.
    let crlf: Vec<(String, String)> = v
        .iter()
        .take(10)
        .map(|(n, s)| (format!("{n} (CRLF)"), s.replace('\n', "\r\n")))
        .collect();
    v.extend(crlf);
    v
}

/// The edits tried at an element of `kind` with token text `text`.
pub(crate) fn edits(kind: K, text: &str) -> Vec<String> {
    let mut v = vec!["rm".to_owned(), "cp".to_owned(), "rb".to_owned()];
    for f in [
        "\n", " ", "# c\n", "x", "\t", "\n  ", "- y\n", "---\n", "...\n",
    ] {
        v.push(format!("ib{f}"));
    }
    for f in ["\n", " ", "--- y\n", "...\n"] {
        v.push(format!("ia{f}"));
    }
    // A move before the previous sibling, and a rolled-back group.
    v.push("mv".into());
    v.push("gr".into());
    match kind {
        K::Comment => {
            for r in ["# r10", "#", "# c"] {
                v.push(format!("rt{r}"));
            }
        }
        K::Whitespace => {
            let n = text.len();
            if n > 1 {
                v.push(format!("rt{}", " ".repeat(n - 1)));
            }
            v.push(format!("rt{}", " ".repeat(n + 2)));
            v.push("rt    ".into());
            if text.contains(' ') {
                v.push(format!("rt{}", text.replacen(' ', "\t", 1)));
            }
            if text.contains('\t') {
                v.push(format!("rt{}", text.replace('\t', " ")));
            }
        }
        K::LineBreak => {
            v.push("rt\n".into());
            v.push("rt\r\n".into());
        }
        K::PlainScalar => {
            for r in ["v", "a: b", "x y", "zzzz"] {
                v.push(format!("rt{r}"));
            }
        }
        K::SingleQuotedScalar => v.push("rt'q'".into()),
        K::DoubleQuotedScalar => v.push("rt\"q\"".into()),
        k if !k.is_token() => {
            v.push("rpv".into());
            v.push("rp[a]".into());
        }
        _ => {}
    }
    v
}

/// Applies edit `e` at `id`: `Some(accepted)`, or `None` when its
/// fragment does not parse under `s`.
pub(crate) fn apply(t: &mut SyntaxTree, id: NodeId, e: &str, s: &Settings) -> Option<bool> {
    let frag = |f: &str| Fragment::parse_with(f, s).ok();
    let r = if let Some(f) = e.strip_prefix("ib") {
        t.insert_before(id, frag(f)?).map(|_| ())
    } else if let Some(f) = e.strip_prefix("ia") {
        t.insert_after(id, frag(f)?).map(|_| ())
    } else if let Some(r) = e.strip_prefix("rt") {
        t.replace_token(id, r).map(|_| ())
    } else if let Some(f) = e.strip_prefix("rp") {
        t.replace(id, frag(f)?).map(|_| ())
    } else if e == "mv" {
        // Moved before its previous sibling (a document with errors among
        // others, at stream level).
        let prev = t.get(id).ok()?.prev_sibling()?.id();
        let f = match t.remove(id) {
            Ok(f) => f,
            Err(e) => {
                return Some(e == oneyaml_syntax::tree::TreeEditError::Internal).map(|i| {
                    assert!(!i, "internal error: {e:?}");
                    false
                });
            }
        };
        t.insert_before(prev, f).map(|_| ())
    } else if e == "gr" {
        // Two edits in a group, rolled back: the tree is the one before
        // (compared with a re-parse of its text, as any accepted edit).
        let m = t.begin_group();
        let _ = t.remove(id);
        if let Ok(f) = Fragment::parse_with("\n", s) {
            let root = t.root().id();
            let _ = t.append_child(root, f);
        }
        t.rollback_group(m).map(|_| ())
    } else if e == "cp" {
        // A copy of the element inserted after it.
        let f = t.extract(id, id).ok()?;
        t.insert_after(id, f).map(|_| ())
    } else if e == "rb" {
        // Removed, then put back where it was (the state after the
        // removal alone is compared when putting it back is refused).
        let n = t.get(id).ok()?;
        let (next, parent) = (n.next_sibling().map(|x| x.id()), n.parent()?.id());
        let Ok(f) = t.remove(id) else {
            return Some(false);
        };
        let back = match next {
            Some(x) => t.insert_before(x, f).map(|_| ()),
            None => t.append_child(parent, f).map(|_| ()),
        };
        back.or_else(|e| match e {
            oneyaml_syntax::tree::TreeEditError::Internal => Err(e),
            _ => Ok(()),
        })
    } else {
        t.remove(id).map(|_| ())
    };
    if r == Err(oneyaml_syntax::tree::TreeEditError::Internal) {
        panic!("internal error: {e:?}\n{:?}", t.emit());
    }
    Some(r.is_ok())
}

#[derive(Default)]
struct Tally {
    edits: usize,
    accepted: usize,
    accepted_recovered: usize,
    recovered_inputs: usize,
    failures: Vec<String>,
}

/// The battery at `limit` elements of `src` (all for `None`), sampled
/// with `rng`, under both settings.
fn sweep(name: &str, src: &str, limit: Option<usize>, rng: &mut Rng, t: &mut Tally) {
    let lenient = Settings::new().lenient_inline_colons(true);
    for s in [Settings::new(), lenient] {
        let base = SyntaxTree::parse_with(src, &s);
        let recovered = base.has_errors();
        t.recovered_inputs += usize::from(recovered);
        let all: Vec<(NodeId, K, String)> = base
            .root()
            .descendants()
            .skip(1)
            .map(|n| {
                let text = if n.is_token() {
                    n.text().into_owned()
                } else {
                    String::new()
                };
                (n.id(), n.kind(), text)
            })
            .collect();
        let picked: Vec<usize> = match limit {
            Some(k) if k < all.len() => (0..k).map(|_| rng.below(all.len())).collect(),
            _ => (0..all.len()).collect(),
        };
        for i in picked {
            let (id, kind, text) = &all[i];
            for e in edits(*kind, text) {
                // A fork keeps the handles of the base.
                let mut tree = base.fork();
                t.edits += 1;
                if apply(&mut tree, *id, &e, &s) != Some(true) {
                    continue;
                }
                t.accepted += 1;
                t.accepted_recovered += usize::from(recovered);
                let again = SyntaxTree::parse_with(tree.emit(), &s);
                if let Err(m) = compare_trees(&tree, &again) {
                    let lenient = s.get_lenient_inline_colons();
                    let mut m = m.to_string();
                    m.truncate(600);
                    t.failures.push(format!(
                        "{name} (lenient {lenient}, recovered {recovered}): {e:?} at {kind:?} \
                         {text:?} {:?}\n  edited text {:?}\n  {m}",
                        base.get(*id).unwrap().span(),
                        if src.len() < 400 {
                            tree.emit()
                        } else {
                            "<long>".into()
                        },
                    ));
                }
            }
        }
    }
}

fn report(what: &str, t: &Tally) {
    eprintln!(
        "{what}: {} edits, {} accepted ({} on recovered input), {} recovered input readings, {} \
         accepted edits differ from their re-parse",
        t.edits,
        t.accepted,
        t.accepted_recovered,
        t.recovered_inputs,
        t.failures.len()
    );
    assert!(
        t.failures.is_empty(),
        "{what}: {} accepted edits differ from a fresh parse of their text; the first:\n{}",
        t.failures.len(),
        t.failures
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The inputs found while hardening the edit layer and the synthetic
/// shapes, every element.
#[test]
fn reported_inputs_and_shapes_agree_with_their_reparse() {
    let mut inputs = synthetic();
    for rel in [
        "fuzz/corpus/edit/57dba2daeda550b4dffb3ec8bf840525189465ab",
        "fuzz/corpus/edit/580b09bf5d81b03200b89e81c0470f91c09ff9d8",
        "fuzz/corpus/edit/5e88c17084d24525a32413f85f2a5e5bf7625cfc",
        "fuzz/corpus/edit/c03a0b5c2b91813b686b28b4046d48cd969e6f34",
        "fuzz/corpus/edit/84315ac8d1ed5c2b420f34ffb8075385bc32a817",
        "fuzz/corpus/parse/8c425d11c2fefc3cbd95e0ef31704ca712c3e011",
        "fuzz/corpus/ser/f0a74a38b907fbd1fcb573d89452d44a664448e3",
        "fuzz/corpus/from_slice/76e9bc0dda18cbf0d4b45c7ceda98f660cdeda07",
        "fuzz/corpus/from_slice/d5a9e468f956f2da5e39f770dcde4c60c82d982e",
        "fuzz/corpus/consistency/97d652d35e0875d7c17aa8d83025b8e173a30196",
        "fuzz/corpus/serde/0f725a4ec6ddc702fbb72165fa6b3f08aafb05a9",
        "fuzz/corpus/render/a083f764c658303fb796f3cf4b915e3d6fe1595b",
        "fuzz/regressions/edit/e3-white-space-before-a-bom-2",
        "fuzz/corpus/ser/0cfdc85f04471de15b3be4924a9793ab95ee4792",
    ] {
        let bytes = fs::read(crates().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        inputs.push((rel.to_owned(), String::from_utf8_lossy(&bytes).into_owned()));
    }
    // The amplified consistency input.
    let src = String::from_utf8_lossy(
        &fs::read(
            crates().join("fuzz/corpus/consistency/97d652d35e0875d7c17aa8d83025b8e173a30196"),
        )
        .unwrap(),
    )
    .into_owned();
    for seed in 0..4u64 {
        let mut rng = Rng(0x48c0 + seed);
        let k = 2 + seed as usize;
        inputs.push((
            format!("97d652d3 amplified {seed}"),
            amplify(&src, &mut rng, k),
        ));
    }
    let mut rng = Rng(0x48a1);
    let mut t = Tally::default();
    for (name, src) in &inputs {
        // (Large inputs: 400 elements each, which covers the shapes.)
        let limit = (src.len() > 3000).then_some(400);
        sweep(name, src, limit, &mut rng, &mut t);
    }
    assert!(t.accepted_recovered > 1000, "{}", t.accepted_recovered);
    report("reported inputs and shapes", &t);
}

/// The yaml-test-suite: 30 sampled elements of every input.
#[test]
fn suite_edits_agree_with_their_reparse() {
    let inputs = files(
        &crates().join("test-suite/data/yaml-test-suite"),
        Some("in.yaml"),
    );
    assert!(inputs.len() > 300, "yaml-test-suite data missing");
    let mut rng = Rng(0x48a2);
    let mut t = Tally::default();
    for (name, src) in &inputs {
        sweep(name, src, Some(30), &mut rng, &mut t);
    }
    assert!(t.recovered_inputs > 100, "{}", t.recovered_inputs);
    report("yaml-test-suite", &t);
}

/// Every fuzz corpus and the fuzz regressions (recovered inputs among
/// them), and amplified suite and consistency inputs: 6 sampled elements
/// of each (the release-mode sweep of the unit's report covered every
/// element).
#[test]
fn corpus_edits_agree_with_their_reparse() {
    let mut inputs = files(&crates().join("fuzz/corpus"), None);
    assert!(inputs.len() > 1000, "fuzz corpora missing");
    inputs.extend(files(&crates().join("fuzz/regressions"), None));
    let mut amp = files(
        &crates().join("test-suite/data/yaml-test-suite"),
        Some("in.yaml"),
    );
    amp.extend(files(&crates().join("fuzz/corpus/consistency"), None));
    let mut rng = Rng(0x48a3);
    for (name, src) in amp {
        if src.contains('#') && src.len() <= 2000 {
            let k = 1 + rng.below(6);
            inputs.push((format!("{name} amplified"), amplify(&src, &mut rng, k)));
        }
    }
    let mut t = Tally::default();
    for (name, src) in &inputs {
        sweep(name, src, Some(6), &mut rng, &mut t);
    }
    assert!(t.recovered_inputs > 1000, "{}", t.recovered_inputs);
    assert!(t.accepted_recovered > 5000, "{}", t.accepted_recovered);
    report("fuzz corpora and amplified inputs", &t);
}

/// Documents ending with 31 or more nested
/// empty explicit keys (`? ? … a`, and the indented `k:⏎␠␠? ⏎␠␠␠␠? …`)
/// have more than 64 zero-width elements in a row at their end. Every
/// element is swept (accepted edits agree with their re-parse), and edits
/// whose window reads to the end of such a document are accepted: finding
/// its last token with bytes passed over at most 64 elements, and refused
/// them (92 to 260 correct edits per input at depth 70; 1036 in this
/// sweep).
#[test]
fn deep_empty_keys_agree_and_keep_edits_after_them() {
    let mut inputs = Vec::new();
    for d in [31usize, 40, 70] {
        let flat = "? ".repeat(d) + "a\n";
        let nested: String = std::iter::once("k:\n".to_owned())
            .chain((1..=d).map(|i| format!("{}? \n", "  ".repeat(i))))
            .collect();
        inputs.push((format!("flat {d}"), flat.clone()));
        inputs.push((
            format!("flat {d}, then a document"),
            flat.clone() + "---\nb: c\n",
        ));
        inputs.push((format!("flat {d}, then a comment"), flat + "# c\nz: 1\n"));
        inputs.push((format!("nested {d}"), nested.clone()));
        inputs.push((
            format!("nested {d}, then an entry"),
            nested.clone() + "b: c\n",
        ));
        inputs.push((
            format!("nested {d}, then a document"),
            nested + "---\nb: c\n",
        ));
    }
    let mut rng = Rng(0x48b3);
    let mut t = Tally::default();
    for (name, src) in &inputs {
        sweep(name, src, None, &mut rng, &mut t);
    }
    report("deep empty keys", &t);
    // Edits whose window reads to the end of such a document, pinned: a
    // `...` line inserted before it (the stream window reads the document,
    // then the trivia its last token could read).
    for (d, flat) in [(31usize, false), (70, false), (70, true)] {
        let src: String = if flat {
            "? ".repeat(d) + "a\n"
        } else {
            std::iter::once("k:\n".to_owned())
                .chain((1..=d).map(|i| format!("{}? \n", "  ".repeat(i))))
                .collect()
        };
        let mut tree = SyntaxTree::parse(src.as_str());
        let doc = tree.root().first_child().unwrap().id();
        let r = tree.insert_before(doc, Fragment::parse("...\n").unwrap());
        assert!(r.is_ok(), "depth {d}, flat {flat}: {r:?}");
        assert!(compare_trees(&tree, &SyntaxTree::parse(tree.emit())).is_ok());
    }
}

/// An unclosed quote followed by a byte
/// order mark at the start of a line. The mark ends the document for the
/// stream, but the quote (where marks are allowed, §5.2) reads on to the
/// end of the text or the next line starting with `---` or `...`, so `---`
/// appended to the stream changed the quote's error ("quoted scalar is not
/// closed" became "document marker inside a quoted scalar") and the edit
/// was accepted with the old one; the validator now refuses edits after
/// recovered input with no such line between. The three reported inputs:
/// each edit is accepted or refused, and when accepted equals a re-parse
/// of its text.
///
/// The walk back from the edit once stopped at the
/// first recovered stream child, taken to be read whole by the edit's
/// window, when no document lay between; an older unclosed quote before
/// the next document still read on to the edit (`"b⏎<BOM>x⏎: ]⏎# c` with
/// `---` appended: the clean `x` and the error region `: ]` lie between).
/// The known inputs, a `...` after a mark on its line (a document end
/// for the stream, content for the quote), and an unclosed quote in a
/// failed directive block before `---` (dsynth probe), are refused.
#[test]
fn unclosed_quote_before_a_mark_line() {
    for (src, frag, refused) in [
        ("'a\n\u{feff}---\n", "---\n", false),
        ("'x\n\u{feff}\na\n", "---\n", false),
        ("'x\n\u{feff}\na\n", "# c\n---\n", false),
        ("\"x\n\u{feff}\na\n", "---\n", false),
        ("\"x\n\u{feff}\na\n", "# c\n---\n", false),
        ("\"b\n\u{feff}x\n: ]\n# c\n", "---\n", true),
        ("'b\n\u{feff}x\n: ]\n# c\n", "---\n", true),
        (
            "'q\n\u{feff}---\na\n# c\n  y\n%YAML 1.2\n[a\n",
            "---\na",
            true,
        ),
        (
            "'q\n\u{feff}---\na\n# c\n  y\n%YAML 1.2\n[a\n",
            "--- x\n",
            true,
        ),
        ("'q\n\u{feff}x\n\u{feff}...\n", "---\n", true),
        ("'q\n\u{feff}x\n: ]\n\u{feff}...\n", "---\n", true),
        // The quote in a failed directive block, before the `---` (its
        // document was taken to have directive errors only).
        (
            "\n%YAML 2.0\nc: x\n\u{feff}\n'q\n\n\u{feff}---\n\u{feff}---\n",
            "---\n",
            true,
        ),
        ("%YAML 1.2\nc: x\n'q\n\u{feff}---\n", "---\n", true),
    ] {
        let mut tree = SyntaxTree::parse(src);
        assert!(tree.has_errors(), "{src:?}");
        let root = tree.root().id();
        let accepted = tree
            .append_child(root, Fragment::parse(frag).unwrap())
            .is_ok();
        let again = SyntaxTree::parse(tree.emit());
        if let Err(m) = compare_trees(&tree, &again) {
            panic!("{src:?} + {frag:?} (accepted {accepted}): {m}");
        }
        assert!(!(refused && accepted), "{src:?} + {frag:?} was accepted");
    }
}

/// In a document with a content error, a
/// duplicate `%YAML` (a directive error, before `---`) no longer refuses
/// edits whose window ends before the content error; each accepted edit
/// equals a re-parse. The directive shapes are swept whole as well.
#[test]
fn directive_errors_do_not_block_edits_before_content_errors() {
    let src = "%YAML 1.2\n%YAML 1.2\n---\na: x\nb: ]\n";
    let mut tree = SyntaxTree::parse(src);
    let x = tree
        .root()
        .descendants()
        .find(|n| n.kind() == K::PlainScalar && n.text() == "x")
        .unwrap()
        .id();
    let r = tree.replace_token(x, "z");
    assert!(r.is_ok(), "{r:?}");
    assert!(compare_trees(&tree, &SyntaxTree::parse(tree.emit())).is_ok());
    let mut rng = Rng(0x48b4);
    let mut t = Tally::default();
    for (name, src) in [
        ("duplicate %YAML", src),
        (
            "duplicate %TAG",
            "%TAG !t! a\n%TAG !t! b\n---\n- !t!x [y\n- z\n",
        ),
        ("%YAML 2.0", "%YAML 2.0\n---\nk: v\nb: ]\n"),
        (
            "failed directive block kept with later directives",
            "%YAML 0.2\n----\nm\n...\n%YAML 1.2\n%YAML 1.2\n---\na: x\nb: ]\n",
        ),
    ] {
        sweep(name, src, None, &mut rng, &mut t);
    }
    report("directive errors", &t);
}
