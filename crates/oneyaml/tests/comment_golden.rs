//! Golden attachments: for every rule of `docs/comment-attachment.md`,
//! inputs → the expected (comment, rule, owner, blank lines) of each
//! comment, plus the three real-world shapes (Kubernetes manifest,
//! GitHub Actions workflow, docker-compose with merge keys), against the
//! snapshot `tests/comment_golden.snapshot`.
//!
//! **Approved by the owner (2026-09-30)** (owner assignments are
//! stable within a major version). To regenerate it after an
//! approved rulebook change, run with `ONEYAML_BLESS=1` and review the diff.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use oneyaml::{CommentExplanation, CommentOwner, Document, NodeId, Stream};
use oneyaml_syntax::tree::SyntaxKind;

const SNAPSHOT: &str = "tests/comment_golden.snapshot";

/// `(name, input)`; the name starts with the rule it is about.
const CASES: &[(&str, &str)] = &[
    // Rule 1: before `---`.
    (
        "r01_directives",
        "%YAML 1.2\n# among directives\n---\na: 1\n",
    ),
    (
        "r01_between_documents",
        "a: 1\n# about the next one\n---\nb: 2\n",
    ),
    (
        "r01_blank_lines_do_not_matter",
        "# one\n\n# two\n\n---\nx\n",
    ),
    ("r01_after_end_marker", "a\n...\n# after the end\n---\nb\n"),
    // Rule 1 wins over rule 6 before a `---`:
    // was `r06_across_documents`.
    (
        "r01_commented_out_item_before_start",
        "items:\n  - a\n  # - b\n---\nx: 1\n",
    ),
    // Rule 2: blank line before the root.
    (
        "r02_license_header",
        "# Copyright\n# license text\n\n# about a\na: 1\n",
    ),
    ("r02_after_start_marker", "---\n# header\n\nkey: v\n"),
    ("r02_no_visible_root", "---\n# nothing here\n...\n"),
    (
        "r02_no_visible_root_implicit_end",
        "--- \n# still the header\n",
    ),
    ("r02_no_document", "# only a comment\n"),
    // Rule 3: no blank line before the root.
    ("r03_first_entry", "# about a\na: 1\nb: 2\n"),
    ("r03_first_item", "# first\n- 1\n- 2\n"),
    ("r03_anchored_root", "# the whole mapping\n&x\na: 1\n"),
    ("r03_root_scalar", "# a scalar\nhello\n"),
    ("r03_after_start_marker", "---\n# about k\nk: v\n"),
    // Rule 4: directly above a node.
    ("r04_entry", "a: 1\n# lead b\nb: 2\n"),
    ("r04_nested_first_entry", "a:\n  # lead x\n  x: 1\n"),
    ("r04_item", "- 1\n# lead two\n- 2\n"),
    ("r04_value_below_key", "k:\n  # lead v\n  v\n"),
    (
        "r04_collection_with_properties",
        "k:\n  # the mapping\n  &a\n  x: 1\n",
    ),
    ("r04_explicit_key_line", "?\n  # the entry\n  a\n: b\n"),
    // Rule 5: same line.
    ("r05_entry", "a: 1 # one\nb: two # two\n"),
    ("r05_block_value_entry", "key:  # about key\n  x: 1\n"),
    ("r05_item", "- a # item a\n- - b # inner b\n"),
    ("r05_flow_value_entry", "a: [1, 2] # entry a\n"),
    ("r05_open_bracket", "k: [ # entry k\n  1]\n"),
    (
        "r05_markers",
        "%YAML 1.2 # directive\n--- # start\na\n... # end\n",
    ),
    ("r05_root", "hello # the root\n"),
    ("r05_properties_line", "k: &a !!map # entry k\n  x: 1\n"),
    ("r05_explicit_entry", "? a # key line\n: b # value line\n"),
    ("r05_block_scalar_header", "k: | # entry k\n  text\n"),
    ("r05_flow_items", "[a, # after a\n b # after b\n]\n"),
    // Rule 6: collection footer.
    ("r06_commented_out_item", "list:\n  - a\n  # - b\nnext: 1\n"),
    (
        "r06_nested_columns",
        "a:\n  b:\n    - 1\n    # c4\n  # c2\n# c0\nd: 1\n",
    ),
    ("r06_root_footer_deeper", "a: 1\n  # deeper than the root\n"),
    ("r06_deeper_than_all", "a:\n    b: 1\n  # c\nd: 1\n"),
    (
        "r06_after_last_block_scalar",
        "m:\n  a: |\n    text\n  # foot of m\nn: 1\n",
    ),
    ("r06_blank_lines_kept", "a:\n  - 1\n\n  # - 2\n\nb: 1\n"),
    // Rule 7: document footer.
    ("r07_footer", "a: 1\n# footer\n"),
    ("r07_before_end_marker", "a: 1\n# f\n...\n"),
    ("r07_after_end_marker", "a\n...\n# after end\n"),
    ("r07_flow_root", "[1, 2]\n  # flow root has no footer\n"),
    (
        "r07_after_last_block_scalar",
        "a: 1\nb: |\n  text\n# document footer\n",
    ),
    (
        "r07_keep_chomping",
        "k: |+\n  a\n\n# kept blank line is content\n",
    ),
    // Rule 8: blank lines.
    (
        "r08_blank_lines",
        "a: 1\n\n\n# two blank lines\nb: 2\n  \n# one blank line\nc: 3\n",
    ),
    ("r08_crlf", "# h\r\n\r\na: 1 # t\r\n# f\r\n"),
    // Rule 9: flow collections.
    (
        "r09_sequence",
        "[\n  # lead a\n  a,\n  # lead b\n  b\n  # footer\n]\n",
    ),
    ("r09_mapping", "{a: 1,\n # lead b\n b: 2\n # footer\n}\n"),
    ("r09_comma_skipped", "[a\n# lead b\n, b]\n"),
    ("r09_nested", "k: [[a,\n  # inner footer\n  ], b]\n"),
    // Rule 10: shallower than the node below.
    ("r10_continuing_mapping", "a:\n  b: 1\n# shallow\n  d: 2\n"),
    ("r10_value_below_key", "k:\n# shallow v\n  v\n"),
    // Rule 11: explicit entries.
    ("r11_value", "? a\n# the value\n: b\n"),
    ("r11_empty_value", "? a\n# the empty value\n:\n"),
    // Rule 12: inside a node.
    (
        "r12_between_properties",
        "k: &a\n  # between properties\n  !!map\n  x: 1\n",
    ),
    (
        "r12_header_line",
        "k: !!str\n  # above the header\n  |\n  text\n",
    ),
    // Rule 13: owners only move outward within a run.
    (
        "r13_inner_footer_after_outer",
        "a:\n  b:\n    c: 1\n  # c2\n    # c4\n",
    ),
    (
        "r13_footer_after_next_leading",
        "a:\n  - 1\n# lead b\n  # - 2\nb: 1\n",
    ),
    (
        "r13_footer_after_document_footer",
        "a:\n  - 1\n# lead b\n\n  # - 2\n",
    ),
    (
        "r13_run_continues_outward",
        "a:\n  b:\n    - 1\n    # c4\n  # c2\n    # c4 again\n# c0\n  # c2 again\nd: 1\n",
    ),
    // Block scalars (comment rule 4 vs 6/7).
    ("bs_middle_entry", "a: |\n  text\n# lead b\nb: 1\n"),
    ("bs_middle_item", "- |\n  text\n# lead two\n- 2\n"),
    // Freeze-gate shapes.
    ("gate_kubernetes", K8S),
    ("gate_github_actions", ACTIONS),
    ("gate_compose_merge_keys", COMPOSE),
];

const K8S: &str = "\
# Deployment for the web tier
# (managed by the platform team)

apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    app: web # selector label
    # tier: frontend
spec:
  replicas: 2 # scaled by the HPA
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
    spec:
      containers:
        # the main container
        - name: nginx
          image: \"nginx:1.14.2\"
          ports:
            - containerPort: 80
            # - containerPort: 443
---
# The service in front of it
apiVersion: v1
kind: Service
spec:
  ports: [{port: 80, targetPort: 8080}] # http only
";

const ACTIONS: &str = "\
name: CI
on:
  push:
    branches: [main]
  # pull_request:
  #   branches: [main]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@v4
    # build it
    - name: Build
      run: |
        cargo build --verbose
        cargo doc
    # then test it
    - name: Test
      run: cargo test # all targets
";

const COMPOSE: &str = "\
# docker-compose with shared settings
x-defaults: &defaults # anchored
  restart: always
  # logging: json

services:
  web:
    <<: *defaults # merge the defaults
    image: nginx
    ports:
      - \"80:80\" # public
  db:
    <<: *defaults
    # the database
    image: postgres
# end of services
";

fn line_col(src: &str, at: usize) -> String {
    let before = &src[..at];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map_or(at, |i| at - i - 1) + 1;
    format!("{line}:{col}")
}

fn describe(doc: &Document, src: &str, id: NodeId) -> String {
    let n = doc.node(id).unwrap();
    let span = n.span();
    let text = src[span.start..span.end].lines().next().unwrap_or("");
    let short: String = text.chars().take(24).collect();
    format!("{:?} {short:?} @{}", n.kind(), line_col(src, span.start))
}

fn render_one(doc: &Document, src: &str, e: &CommentExplanation) -> String {
    let owner = match e.owner() {
        CommentOwner::DocumentHeader => "document header".to_owned(),
        CommentOwner::DocumentFooter => "document footer".to_owned(),
        CommentOwner::Leading(id) => format!("leading {}", describe(doc, src, id)),
        CommentOwner::Trailing(id) => format!("trailing {}", describe(doc, src, id)),
        CommentOwner::Footer(id) => format!("footer {}", describe(doc, src, id)),
        o => panic!("unknown owner {o:?}"),
    };
    format!(
        "{} {:?} -> rule {} {owner} (blank {})\n",
        line_col(src, e.span().start),
        e.text(),
        e.rule().number(),
        e.blank_lines_before()
    )
}

/// Each document's explanations (a stream without documents: the whole
/// input's).
fn explain(src: &str) -> Vec<(Document, Vec<CommentExplanation>)> {
    let stream = Stream::parse(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let docs: Vec<Document> = if stream.is_empty() {
        vec![Document::parse(src).unwrap()]
    } else {
        stream.documents().collect()
    };
    docs.into_iter()
        .map(|d| {
            let e = d.explain_comments().unwrap();
            (d, e)
        })
        .collect()
}

fn render() -> String {
    let mut out = String::from(
        "# oneyaml comment attachment goldens (docs/comment-attachment.md).\n\
         # Approved baseline (2026-09-30). Each case: `=== name`, the input (`| `), then per\n\
         # document each comment: line:col \"text\" -> rule N owner (blank lines).\n",
    );
    for (name, src) in CASES {
        let _ = writeln!(out, "=== {name}");
        for line in src.split_inclusive('\n') {
            let _ = writeln!(out, "| {}", line.trim_end_matches(['\n', '\r']));
        }
        let docs = explain(src);
        let many = docs.len() > 1;
        for (i, (d, es)) in docs.iter().enumerate() {
            if many {
                let _ = writeln!(out, "-- document {i}");
            }
            for e in es {
                out.push_str(&render_one(d, src, e));
            }
        }
    }
    out
}

fn parse(snapshot: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in snapshot.split_inclusive('\n') {
        if let Some(name) = line.strip_prefix("=== ") {
            if let Some((n, body)) = current.take() {
                map.insert(n, body);
            }
            current = Some((name.trim_end_matches('\n').to_owned(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
        }
    }
    if let Some((n, body)) = current {
        map.insert(n, body);
    }
    map
}

#[test]
fn golden_attachments_match_the_snapshot() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
    let rendered = render();
    if std::env::var_os("ONEYAML_BLESS").is_some() {
        std::fs::write(&path, &rendered).unwrap();
    }
    let frozen = parse(&std::fs::read_to_string(&path).expect("tests/comment_golden.snapshot"));
    let current = parse(&rendered);
    let mut diffs = Vec::new();
    for (name, body) in &current {
        match frozen.get(name) {
            Some(f) if f.as_bytes() == body.as_bytes() => {}
            Some(f) => diffs.push(format!("{name}:\n--- snapshot\n{f}--- now\n{body}")),
            None => diffs.push(format!("{name}: not in the snapshot")),
        }
    }
    for name in frozen.keys() {
        if !current.contains_key(name) {
            diffs.push(format!("{name}: in the snapshot but no longer a case"));
        }
    }
    assert!(
        diffs.is_empty(),
        "attachments changed:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn every_rule_has_goldens_and_each_case_shows_its_rule() {
    let mut fired = BTreeSet::new();
    let mut blank = false;
    for (name, src) in CASES {
        let mut here = BTreeSet::new();
        for (_, es) in explain(src) {
            for e in es {
                here.insert(e.rule().number());
                blank |= e.blank_lines_before() > 0;
            }
        }
        // A case named for rule N shows rule N.
        if let Some(n) = name.strip_prefix('r').and_then(|r| r.get(..2)) {
            let n: u8 = n.parse().unwrap();
            if n != 8 {
                assert!(
                    here.contains(&n),
                    "{name}: rule {n} does not fire ({here:?})"
                );
            }
        }
        fired.extend(here);
    }
    assert_eq!(
        fired.into_iter().collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13],
        "every owner rule has goldens"
    );
    assert!(blank, "rule 8 (blank lines) has goldens");
}

#[test]
fn every_comment_of_every_case_is_explained_once() {
    for (name, src) in CASES {
        let docs = explain(src);
        let total: usize = docs.iter().map(|(_, e)| e.len()).sum();
        // Through the escape hatch, to count the tree's comments.
        let comments = docs[0]
            .0
            .syntax()
            .root()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::Comment);
        assert_eq!(total, comments.count(), "{name}");
        // The single-comment entry point agrees with the pass.
        for (d, es) in &docs {
            for e in es {
                assert_eq!(&d.explain_comment(e.id()).unwrap(), e, "{name}");
            }
        }
    }
}
