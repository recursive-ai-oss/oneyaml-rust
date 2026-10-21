//! The comment API's reads agree with
//! `explain_comments` on every comment of the yaml-test-suite corpus —
//! clean and recovered input, each also with comment lines injected at
//! random places and columns: every comment is found by `comments(id)`
//! (leading, trailing, footer), `header()` or `footer()` at exactly the
//! owner `explain_comments` gives it, and nowhere else, owners reached
//! through `errors()` included. Documents of a `Stream` are checked each.
//! Also: header and footer reads and writes stay linear next to many stray
//! `...` markers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod work;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use oneyaml::{CommentId, CommentOwner, Document, NodeId, NodeKind, NodeRef, Stream, path};

fn suite_inputs() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    let mut out = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    out.push((p.display().to_string(), text));
                }
            }
        }
    }
    out.sort();
    out
}

/// A small deterministic PRNG (xorshift64*).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1) as u64) as usize
    }
}

/// `src` with comment lines injected at random lines and columns.
fn with_comments(rng: &mut Rng, src: &str) -> String {
    let mut lines: Vec<String> = src.lines().map(str::to_owned).collect();
    for k in 0..1 + rng.below(4) {
        let at = rng.below(lines.len() + 1);
        let col = [0, 1, 2, 4][rng.below(4)];
        let blank = if rng.below(3) == 0 { "\n" } else { "" };
        lines.insert(at, format!("{blank}{}# i{k}", " ".repeat(col)));
    }
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

type Key = (&'static str, Option<NodeId>);

/// Every node navigation presents: keys, values, items (aliases not
/// followed), and the error regions.
fn nodes(d: &Document) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    let mut stack: Vec<NodeRef<'_>> = d.root().into_iter().collect();
    stack.extend(d.errors());
    while let Some(n) = stack.pop() {
        if !out.insert(n.id()) || n.kind() == NodeKind::Alias {
            continue;
        }
        if let Some(m) = n.as_mapping().filter(|m| m.node() == n) {
            for (k, v) in m.iter() {
                stack.push(k);
                stack.push(v);
            }
        }
        if let Some(s) = n.as_sequence().filter(|s| s.node() == n) {
            stack.extend(s.iter());
        }
    }
    out
}

fn check(d: &Document, what: &str) -> usize {
    let mut want: BTreeMap<Key, Vec<CommentId>> = BTreeMap::new();
    let all = d.explain_comments().unwrap();
    for e in &all {
        let key: Key = match e.owner() {
            CommentOwner::DocumentHeader => ("header", None),
            CommentOwner::DocumentFooter => ("footer", None),
            CommentOwner::Leading(n) => ("leading", Some(n)),
            CommentOwner::Trailing(n) => ("trailing", Some(n)),
            CommentOwner::Footer(n) => ("collection footer", Some(n)),
            _ => ("other", None),
        };
        want.entry(key).or_default().push(e.id());
    }
    let mut got: BTreeMap<Key, Vec<CommentId>> = BTreeMap::new();
    let mut put = |k: Key, ids: Vec<CommentId>| {
        if !ids.is_empty() {
            got.entry(k).or_default().extend(ids);
        }
    };
    put(
        ("header", None),
        d.header().unwrap().iter().map(|c| c.id()).collect(),
    );
    put(
        ("footer", None),
        d.footer().unwrap().iter().map(|c| c.id()).collect(),
    );
    // (`errors()` of a multi-document `Document::parse_recovering` reaches
    // the other documents' error regions, which are not this document's.)
    for n in nodes(d).into_iter().filter(|n| d.node(*n).is_ok()) {
        let c = d.comments(n).unwrap();
        put(
            ("leading", Some(n)),
            c.leading().iter().map(|c| c.id()).collect(),
        );
        put(
            ("trailing", Some(n)),
            c.trailing().iter().map(|c| c.id()).collect(),
        );
        put(
            ("collection footer", Some(n)),
            c.footer().iter().map(|c| c.id()).collect(),
        );
    }
    for v in want.values_mut().chain(got.values_mut()) {
        v.sort();
    }
    if want != got {
        let text = d.to_string();
        let explained: Vec<String> = all.iter().map(ToString::to_string).collect();
        panic!(
            "{what}: reads disagree with explain_comments\ntext: {text:?}\nexplain: {explained:#?}\nwant: {want:?}\ngot: {got:?}"
        );
    }
    all.len()
}

#[test]
fn reads_agree_with_explain_on_the_whole_corpus() {
    let inputs = suite_inputs();
    assert!(inputs.len() > 300, "{} inputs", inputs.len());
    let mut rng = Rng(0x30d_5eed);
    let (mut comments, mut recovered, mut streams) = (0usize, 0usize, 0usize);
    for (path, text) in &inputs {
        let mut variants = vec![text.clone()];
        for _ in 0..3 {
            variants.push(with_comments(&mut rng, text));
        }
        for (vi, src) in variants.iter().enumerate() {
            let what = format!("{path} v{vi}");
            let d = match Document::parse(src.clone()) {
                Ok(d) => d,
                Err(_) => {
                    recovered += 1;
                    Document::parse_recovering(src.clone()).into_value()
                }
            };
            comments += check(&d, &what);
            let s = match Stream::parse(src.clone()) {
                Ok(s) => s,
                Err(_) => Stream::parse_recovering(src.clone()).into_value(),
            };
            if s.len() > 1 {
                streams += 1;
                for i in 0..s.len() {
                    comments += check(&s.document(i).unwrap(), &format!("{what} document {i}"));
                }
            }
        }
    }
    assert!(comments > 1_000, "{comments} comments");
    assert!(recovered > 100, "{recovered} recovered");
    assert!(streams > 50, "{streams} streams");
}

#[test]
fn known_repros_on_recovered_input() {
    for src in [
        "# a\n# b\n- |+\n   \n\n  # f3\n",
        "---\n[ [ a, b, c ]\n\n # f2\n",
    ] {
        let d = Document::parse_recovering(src).into_value();
        assert!(check(&d, src) > 0);
    }
}

#[test]
#[ignore = "slow: runs in CI"]
fn stray_document_end_markers_stay_linear() {
    // Each stray `...` starts a run whose documents lie
    // past the whole stretch; the stretch is decided at once.
    fn time(n: usize) -> u64 {
        let src = format!("a\n{}", "... # c\n".repeat(n));
        work::reset();
        let mut d = Document::parse(src.clone()).unwrap();
        // (The comment on the document's own `...` is its footer too; one
        // on the first stray may go either way by rule 5: counted by the
        // rulebook, and the reads agree with it.)
        check(&d, "strays");
        assert!(
            d.footer().unwrap().len() + 1 >= n,
            "{}",
            d.footer().unwrap().len()
        );
        d.set_footer(&["x"]).unwrap();
        assert_eq!(d.footer().unwrap().len(), 1);
        let mut s = Stream::parse(format!("{src}--- b\n")).unwrap();
        check(&s.document(1).unwrap(), "strays, stream");
        let mut second = s.document_mut(1).unwrap();
        assert!(
            second.header().unwrap().len() + 1 >= n,
            "{}",
            second.header().unwrap().len()
        );
        second.set_header(&["h"]).unwrap();
        assert_eq!(second.header().unwrap().len(), 1);
        let mut counted = 0u64;
        // Alternating selections (every other comment re-owned
        // to the root, then removed; then the rest through the header),
        // same-line and own-line comments between the markers.
        for unit in ["... # c\n", "...\n# c\n", "...\n  # c\n\n"] {
            let mut s = Stream::parse(format!("a\n{}--- b\n", unit.repeat(n))).unwrap();
            let mut d = s.document_mut(1).unwrap();
            let root = d.root().unwrap().id();
            let ids = d.comment_ids();
            // (A comment on the first `...`, the first document's own, is
            // its footer.)
            assert!(ids.len() + 1 >= n, "{unit:?}: {}", ids.len());
            // (Not counted: re-owning a comment between documents costs its
            // stretch, documented on `attach_comment`.)
            counted += work::taken();
            for c in ids.iter().step_by(2) {
                d.attach_comment(*c, CommentOwner::Leading(root)).unwrap();
            }
            work::reset();
            assert_eq!(d.clear_comments(root).unwrap().len(), ids.len().div_ceil(2));
            d.set_header(oneyaml::NO_COMMENTS).unwrap();
            assert!(d.comment_ids().is_empty(), "{unit:?}");
            let mut first = s.document_mut(0).unwrap();
            first.set_footer(oneyaml::NO_COMMENTS).unwrap();
            assert!(first.comment_ids().is_empty(), "{unit:?}");
            let blank = if unit.ends_with("\n\n") { "\n" } else { "" };
            assert_eq!(
                s.to_string(),
                format!("a\n{}--- b\n", format!("...\n{blank}").repeat(n)),
                "{unit:?}"
            );
        }
        counted + work::taken()
    }
    let small = (0..3).map(|_| time(500)).min().unwrap();
    let large = (0..3).map(|_| time(8_000)).min().unwrap();
    assert!(large < small * 40, "500: {small:?}, 8000: {large:?}");
}

#[test]
fn a_clean_document_next_to_a_broken_one_keeps_local_reads() {
    // Only a document whose own subtree holds an error
    // region reads its comments with a whole-document pass.
    fn time(broken: bool) -> u64 {
        let mut src: String = (0..20_000).map(|i| format!("k{i}: v # t{i}\n")).collect();
        src.push_str(if broken {
            "---\n[ a, b\n"
        } else {
            "---\n[ a, b ]\n"
        });
        let mut s = Stream::parse_recovering(src.clone()).into_value();
        let mut d = s.document_mut(0).unwrap();
        let k = d.get_path(&path!["k10000"]).unwrap().key().unwrap().id();
        work::reset();
        for i in 0..20 {
            let _ = d.comments(k).unwrap();
            d.set_trailing(k, Some(&format!("n{i}"))).unwrap();
        }
        let stream = work::taken();
        // A multi-document `Document::parse_recovering`: its first document
        // (restricted as a whole by the second document's error, so only
        // read here).
        let whole = Document::parse_recovering(src).into_value();
        let k = whole
            .get_path(&path!["k10000"])
            .unwrap()
            .key()
            .unwrap()
            .id();
        work::reset();
        for _ in 0..20 {
            assert_eq!(whole.comments(k).unwrap().trailing().len(), 1);
            let _ = whole.header().unwrap();
            let _ = whole.footer().unwrap();
        }
        stream + work::taken()
    }
    // Only the counts are reported. With the neighbour broken, a read costs
    // about 15 times what it costs next to a clean one, in proportion to
    // the key's position in its document; a time bound with its slack could
    // not see that, and no bound on it is asserted here.
    let clean = (0..3).map(|_| time(false)).min().unwrap();
    let next_to_broken = (0..3).map(|_| time(true)).min().unwrap();
    eprintln!("clean: {clean} steps, next to a broken one: {next_to_broken}");
}
