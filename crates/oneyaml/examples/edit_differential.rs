//! The edit differential (no regressions): one fixed set of
//! edit calls — remove, remove and re-insert at the same place, moves,
//! inserts, replacements, anchor edits — over every entry of every
//! collection of the yaml-test-suite inputs and of commented variants of
//! them (comment lines at several columns, blank lines, same-line
//! comments), in LF, CRLF and CR (also moves to a middle index,
//! re-insertions into another collection of the same kind, and
//! `remove_keep_comments`). Prints one line per call: `key<TAB>outcome`
//! (`OK`, or `E:<kind>` with an `Unsupported` reason).
//!
//! `remove_keep_comments` does not exist in the baseline revision: built with
//! `--cfg edit_baseline` (as the baseline is), its calls print `NA`.
//!
//! A call that must keep the document's meaning (a re-insertion at the same
//! place, a move, a cross-collection re-insertion, anchor edits, the footer
//! rewrite) and is accepted but changes it (the sorted scalar values of the
//! document's `Value`, keys included, differ) prints `OK:meaning`: a wrong
//! result, which a refusal may replace (the baseline's result was wrong).
//! Accepted now with a changed meaning is a regression.
//!
//! The file compiles against the `oneyaml` API of the baseline revision
//! and later, so it runs on both sides:
//!
//! ```text
//! REV=1319da5   # the baseline revision
//! git archive $REV | tar -x -C /tmp/head
//! cp crates/oneyaml/examples/edit_differential.rs /tmp/head/crates/oneyaml/examples/
//! (cd /tmp/head && RUSTFLAGS='--cfg edit_baseline' cargo run --release -p oneyaml \
//!     --example edit_differential -- $PWD/crates/test-suite/data/yaml-test-suite) > head.tsv
//! ```
//!
//! `bench/tests/head_differential.rs` runs [`run`] on the current code and
//! compares it with the committed refusals of the baseline
//! (`bench/data/edit-differential-baseline.tsv`). Loops only, no recursion.
#![allow(
    unexpected_cfgs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    dead_code
)]

use oneyaml::{Document, EditError, NodeId, NodeKind, Value};

/// The document's meaning as the sorted scalars of its `Value` (keys
/// included; order-free, so a move keeps it). `None` when it has none, or
/// when it has aliases (an edit inside an aliased collection shows up
/// once per alias: not a meaning a multiset can check).
pub fn meaning(d: &Document) -> Option<Vec<String>> {
    let mut nodes: Vec<_> = d.root().into_iter().collect();
    while let Some(n) = nodes.pop() {
        if n.kind() == NodeKind::Alias {
            return None;
        }
        if let Some(m) = n.as_mapping().filter(|m| m.node() == n) {
            for (k, v) in m.iter() {
                nodes.push(k);
                nodes.push(v);
            }
        } else if let Some(q) = n.as_sequence().filter(|q| q.node() == n) {
            nodes.extend(q.iter());
        }
    }
    let v = d.to_value().ok()?;
    let mut out = Vec::new();
    let mut stack = vec![&v];
    while let Some(v) = stack.pop() {
        match v {
            Value::Sequence(q) => stack.extend(q.iter()),
            Value::Mapping(m) => {
                for (k, x) in m {
                    stack.push(k);
                    stack.push(x);
                }
            }
            Value::Tagged(t) => {
                out.push(format!("{:?}", t.tag));
                stack.push(&t.value);
            }
            other => out.push(format!("{other:?}")),
        }
    }
    out.sort();
    Some(out)
}

/// [`outcome`], and `OK:meaning` when an accepted call that must keep the
/// meaning changed it.
fn kept<T>(base: &Document, d: &Document, r: Result<T, EditError>) -> String {
    let o = outcome(r);
    if o == "OK" && meaning(d) != meaning(base) {
        "OK:meaning".to_owned()
    } else {
        o
    }
}

/// Whether the document's diagnostics (codes and spans) are those a
/// parse of its text gives (for the `e8` documents).
fn reads_back(d: &Document) -> bool {
    let key = |d: &Document| -> Vec<String> {
        d.diagnostics()
            .iter()
            .map(|x| format!("{:?} {:?}", x.code, x.primary.span))
            .collect()
    };
    key(d) == key(&Document::parse_recovering(d.to_string()).into_value())
}

/// [`kept`] for an insertion of `new` (YAML text): the meaning gains
/// exactly `new`'s scalars.
fn added<T>(base: &Document, d: &Document, new: &str, r: Result<T, EditError>) -> String {
    let o = outcome(r);
    let want = match (
        meaning(base),
        Document::parse(new).ok().and_then(|n| meaning(&n)),
    ) {
        (Some(mut b), Some(n)) => {
            b.extend(n);
            b.sort();
            Some(b)
        }
        _ => None,
    };
    if o == "OK" && want.is_some() && meaning(d) != want {
        "OK:meaning".to_owned()
    } else {
        o
    }
}

/// [`kept`] for a removal: the meaning only loses scalars (every value
/// left keeps its own).
fn removed<T>(base: &Document, d: &Document, r: Result<T, EditError>) -> String {
    let o = outcome(r);
    let sub = match (meaning(base), meaning(d)) {
        (Some(b), Some(n)) => {
            let mut have: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for x in b {
                *have.entry(x).or_default() += 1;
            }
            n.into_iter().all(|x| match have.get_mut(&x) {
                Some(c) if *c > 0 => {
                    *c -= 1;
                    true
                }
                _ => false,
            })
        }
        _ => true,
    };
    if o == "OK" && !sub {
        "OK:meaning".to_owned()
    } else {
        o
    }
}

/// Every collection of the document, with its entries' value handles, in
/// a deterministic order (depth first, from the root; aliases not
/// followed).
pub fn collections(doc: &Document) -> Vec<(NodeId, bool, Vec<NodeId>)> {
    let mut out = Vec::new();
    let mut stack: Vec<_> = doc.root().into_iter().collect();
    while let Some(n) = stack.pop() {
        if n.kind() == NodeKind::Alias {
            continue;
        }
        if let Some(m) = n.as_mapping().filter(|m| m.node() == n) {
            let mut vs = Vec::new();
            for (k, v) in m.iter() {
                stack.push(k);
                stack.push(v);
                vs.push(v.id());
            }
            out.push((n.id(), true, vs));
        } else if let Some(s) = n.as_sequence().filter(|s| s.node() == n) {
            let vs: Vec<NodeId> = s.iter().map(|v| v.id()).collect();
            stack.extend(s.iter());
            out.push((n.id(), false, vs));
        }
    }
    out
}

fn outcome<T>(r: Result<T, EditError>) -> String {
    match r {
        Ok(_) => "OK".to_owned(),
        Err(EditError::Unsupported { reason, .. }) => format!("E:Unsupported:{reason}"),
        Err(e) => {
            let s = format!("{e:?}");
            let kind = s.split([' ', '{', '(']).next().unwrap_or("");
            // (`CommentReattaches { rule: X }`: the rule too.)
            match s.split_once("rule: ") {
                Some((_, r)) => format!("E:{kind}:{}", r.trim_end_matches([' ', '}'])),
                None => format!("E:{kind}"),
            }
        }
    }
}

/// `remove_keep_comments` of the entry whose value is `v` (`NA`
/// in the baseline revision, where it does not exist).
#[cfg(not(edit_baseline))]
fn keep_comments(base: &Document, v: NodeId) -> String {
    let mut d = base.clone();
    outcome(d.remove_keep_comments(v))
}

#[cfg(edit_baseline)]
fn keep_comments(_: &Document, _: NodeId) -> String {
    "NA".to_owned()
}

/// A deterministic generator (the same on both sides).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

/// `src` with comment lines (random columns, some after blank lines) and
/// same-line comments on plain lines.
pub fn commented(seed: u64, src: &str) -> String {
    let mut g = Lcg(seed);
    let mut lines: Vec<String> = src.lines().map(str::to_owned).collect();
    for l in &mut lines {
        let plain = !l.contains(['#', '\'', '"', '|', '>', '[', '{', '\t']) && !l.trim().is_empty();
        if plain && g.below(3) == 0 {
            l.push_str(" # t");
        }
    }
    for _ in 0..(1 + g.below(6)) {
        let at = g.below(lines.len() + 1);
        let col = [0, 1, 2, 4, 6][g.below(5)];
        lines.insert(at, format!("{}# c{at}", " ".repeat(col)));
        if g.below(3) == 0 {
            lines.insert(at, String::new());
        }
    }
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// The calls on one document (`name` prefixes the keys).
pub fn calls(name: &str, src: &str, out: &mut Vec<(String, String)>) {
    // The `e8` extras have error regions (restricted mode).
    let base = match Document::parse(src) {
        Ok(d) => d,
        Err(_) if name.starts_with("e8") => Document::parse_recovering(src).into_value(),
        Err(_) => return,
    };
    // The footer rewritten next to a kept scalar and the final
    // line of spaces.
    if name.starts_with("e5") {
        let mut d = base.clone();
        let r = d.set_footer(&["f"]);
        out.push((format!("{name}#d:footer"), kept(&base, &d, r)));
    }
    let colls = collections(&base);
    // The root node anchored and un-anchored (rule 12: a root
    // collection below comment lines of its first entry).
    if let Some(root) = base.root().map(|r| r.id()) {
        let mut d = base.clone();
        let r = d.set_anchor(root, Some("zq"));
        out.push((format!("{name}#r:ancroot"), kept(&base, &d, r)));
        let mut d = base.clone();
        let r = d.set_anchor(root, None);
        out.push((format!("{name}#r:unancroot"), kept(&base, &d, r)));
    }
    for (ci, (coll, map, values)) in colls.clone().into_iter().enumerate() {
        let n = values.len();
        // In the `e8` documents (error regions), an accepted call
        // whose diagnostics are not those its text reads prints `OK:nn3`
        // (a wrong result, as `OK:meaning`).
        let mut put = |op: String, r: String, d: &Document| {
            let r = if r == "OK" && name.starts_with("e8") && !reads_back(d) {
                "OK:nn3".to_owned()
            } else {
                r
            };
            out.push((format!("{name}#{ci}:{op}"), r));
        };
        for (i, v) in values.iter().enumerate() {
            let mut d = base.clone();
            let r = d.remove_node(*v);
            put(format!("rm{i}"), removed(&base, &d, r), &d);
            let mut d = base.clone();
            let r = d.remove_node(*v).and_then(|r| {
                d.node_mut(coll).and_then(|mut c| {
                    c.insert_removed_at(i, r)
                        .map(|n| n.id())
                        .map_err(|e| e.into_error())
                })
            });
            put(format!("rt{i}"), kept(&base, &d, r), &d);
            let mut tos = vec![0, n.saturating_sub(1)];
            // A middle index too.
            if n > 2 && !tos.contains(&(n / 2)) {
                tos.push(n / 2);
            }
            for to in tos {
                if to != i {
                    let mut d = base.clone();
                    let r = d.node_mut(coll).and_then(|mut c| c.move_entry(i, to));
                    put(format!("mv{i}>{to}"), kept(&base, &d, r), &d);
                }
            }
            let mut d = base.clone();
            let r = outcome(d.node_mut(*v).and_then(|mut m| m.set(7u8).map(|_| ())));
            put(format!("set{i}"), r, &d);
            let mut d = base.clone();
            let r = outcome(d.node_mut(*v).and_then(|mut m| m.set("x\ny\n").map(|_| ())));
            put(format!("ml{i}"), r, &d);
            put(format!("keep{i}"), keep_comments(&base, *v), &base);
            let mut d = base.clone();
            let r = d.set_anchor(*v, Some("zq"));
            put(format!("anc{i}"), kept(&base, &d, r), &d);
            let mut d = base.clone();
            let r = d.set_anchor(*v, None);
            put(format!("unanc{i}"), kept(&base, &d, r), &d);
        }
        // The first entry re-inserted into the next collection
        // of the same kind (appended).
        let other = colls
            .iter()
            .skip(ci + 1)
            .chain(colls.iter().take(ci))
            .find(|(c, m, _)| *m == map && *c != coll);
        if let (Some((to, _, _)), Some(v)) = (other, values.first()) {
            let mut d = base.clone();
            let r = d.remove_node(*v).and_then(|r| {
                d.node_mut(*to).and_then(|mut c| {
                    c.insert_removed(r)
                        .map(|n| n.id())
                        .map_err(|e| e.into_error())
                })
            });
            put("xins".to_owned(), kept(&base, &d, r), &d);
        }
        if map {
            let mut d = base.clone();
            let r = d
                .node_mut(coll)
                .and_then(|mut c| c.insert("zq_new", 1u8).map(|n| n.id()));
            put("add".to_owned(), added(&base, &d, "zq_new: 1", r), &d);
            let mut d = base.clone();
            let r = d
                .node_mut(coll)
                .and_then(|mut c| c.insert("zq_ml", "a\nb\n").map(|n| n.id()));
            put(
                "addml".to_owned(),
                added(&base, &d, "zq_ml: \"a\\nb\\n\"", r),
                &d,
            );
        } else {
            for i in 0..=n {
                let mut d = base.clone();
                let r = d
                    .node_mut(coll)
                    .and_then(|mut c| c.insert_at(i, "new").map(|n| n.id()));
                put(format!("ins{i}"), added(&base, &d, "- new", r), &d);
            }
            let mut d = base.clone();
            let r = d
                .node_mut(coll)
                .and_then(|mut c| c.push("a\nb\n").map(|n| n.id()));
            put(
                "pushml".to_owned(),
                added(&base, &d, "- \"a\\nb\\n\"", r),
                &d,
            );
        }
    }
}

/// Shapes named in reviews, beyond the suite.
pub const EXTRA: &[(&str, &str)] = &[
    ("r2a", "- - a\n  # c\n  - b\n"),
    ("r2b", "- ? a\n  # c\n  b: 1\n"),
    ("r4a", "a: 1\n\n# section two\n\nb: 2\nc: 3\n"),
    ("r4b", "a: 1\n# about b\n\nb: 2\n"),
    ("r4c", "k:\n  a: 1\n\n  # about b\n\n  b: 2\n"),
    ("u27a", "k: 1 # a\n# c\nm: |\n  x\n# d\n"),
    ("u27b", "k: |\n  text\n# c\nm: 2"),
    ("u27c", "k: >\n  text\n\n# c\n\nm: 2\n"),
    ("keep", "- a\n- b: |+\n    keep\n\n# after\n- c\n"),
    // Column-0 comment lines inside flow collections nested in
    // block collections.
    ("f0a", "- [\n# c\n  b, c]\n"),
    ("f0b", "k: [\n# c\n  b, c, d]\n"),
    ("f0c", "k: {\n# c\n  b: 1, c: 2}\n"),
    ("f0d", "- - [a,\n# c\n    b, c]\n  - [d]\n"),
    ("f0e", "k:\n  - [\n# c\n    b]\n  - [c,\n# d\n    e]\n"),
    ("f0f", "k: {a: 1,\n# c\n  b: 2}\nz: [\n# d\n  y]\n"),
    // Root anchoring below comment lines (rule 12).
    ("r12a", "# about a\na: 1\nb: 2\n"),
    ("r12b", "# h\n\n# about\n- 1\n- 2\n"),
    // Fuzz finding E5: a kept scalar before the text's final unterminated
    // line of spaces (anchors, re-insertions, the footer, moves).
    ("e5a", "a: 1\nk: |+\n  x\n "),
    ("e5b", "- 1\n- |+\n  x\n "),
    ("e5c", "- 1\n- |+\n  x\n# c\n "),
    ("e5d", "k: |+\n  x\n\nz: 1\n "),
    ("e5e", "a:\n  - b\n  - &n !t |+\n    x\n  "),
    // Fuzz finding E8: nodes beside error regions.
    ("e8a", "{a: 1, b: 2} ]\n"),
    ("e8b", "[1, [2, 3]] ]\n"),
    ("e8c", "k: {a: 1, b: 2} ]\nz: 1\n"),
    ("e8d", "[[a, x\n ab]: 1]\n"),
];

/// Every call over the corpus under `dir` (yaml-test-suite `*/in.yaml`,
/// and `*/*/in.yaml`), sorted by key.
pub fn run(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut inputs: Vec<(String, String)> = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((p, id)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                let id = if id.is_empty() {
                    name
                } else {
                    format!("{id}/{name}")
                };
                stack.push((path, id));
            } else if name == "in.yaml" && !p.join("error").exists() {
                if let Ok(s) = std::fs::read_to_string(&path) {
                    inputs.push((id.clone(), s));
                }
            }
        }
    }
    inputs.extend(
        EXTRA
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned())),
    );
    inputs.sort();
    let mut out = Vec::new();
    for (id, src) in &inputs {
        let mut seed = 0x31c_u64;
        for b in id.bytes() {
            seed = seed.wrapping_mul(31).wrapping_add(u64::from(b));
        }
        let variants = [
            ("p", src.clone()),
            ("c1", commented(seed, src)),
            ("c2", commented(seed ^ 0x5eed, src)),
        ];
        for (v, text) in &variants {
            for (nl, tag) in [("\n", "lf"), ("\r\n", "crlf"), ("\r", "cr")] {
                let t = text.replace('\n', nl);
                calls(&format!("{id}/{v}/{tag}"), &t, &mut out);
            }
        }
    }
    out.sort();
    out
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/test-suite/data/yaml-test-suite".to_owned());
    for (k, v) in run(std::path::Path::new(&dir)) {
        println!("{k}\t{v}");
    }
}
