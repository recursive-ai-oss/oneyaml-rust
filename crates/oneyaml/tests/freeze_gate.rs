//! Behaviours the freeze-gate examples (`examples/
//! k8s_manifest.rs`, `gha_workflow.rs`, `compose_merge.rs`) ran into.
//! The API gaps, the tracked gap, and the round-trip defect pinned
//! here are fixed; the pins now hold the fixed behaviour.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, Settings, Stream, Value, path};

/// Was a gap: `insert_alias("<<", ..)` still asks for the
/// *string* key `<<`, written `'<<'` (every-reader quoting), which
/// merges nothing; `insert_merge` writes the merge key. Both readers agree
/// either way.
#[test]
fn merge_keys_are_inserted_with_insert_merge() {
    let settings = Settings::new().merge_keys(true);
    let src = "base: &b\n  image: nginx\nweb:\n  <<: *b\nworker:\n  command: run\n";
    let mut doc = Document::parse_with(src, &settings).unwrap();
    let base = doc.id_of(&path!["base"]).unwrap();
    doc.get_mut("worker")
        .unwrap()
        .insert_alias("<<", base)
        .unwrap();
    let text = doc.to_string();
    assert_eq!(
        text,
        "base: &b\n  image: nginx\nweb:\n  <<: *b\nworker:\n  command: run\n  '<<': *b\n"
    );
    let v: Value = oneyaml::from_str_with(&text, &settings).unwrap();
    assert_eq!(v, doc.to_value().unwrap());
    // `web` merges; `worker` has a string key `<<` instead.
    assert_eq!(v["web"]["image"].as_str(), Some("nginx"));
    assert!(v["worker"]["image"].is_null());
    assert!(v["worker"]["<<"].is_mapping());
    // The merge key itself.
    let mut doc = Document::parse_with(src, &settings).unwrap();
    let base = doc.id_of(&path!["base"]).unwrap();
    doc.get_mut("worker")
        .unwrap()
        .insert_merge(&[base])
        .unwrap();
    let text = doc.to_string();
    assert_eq!(
        text,
        "base: &b\n  image: nginx\nweb:\n  <<: *b\nworker:\n  <<: *b\n  command: run\n"
    );
    let v: Value = oneyaml::from_str_with(&text, &settings).unwrap();
    assert_eq!(v, doc.to_value().unwrap());
    assert_eq!(v["worker"]["image"].as_str(), Some("nginx"));
    assert_eq!(
        doc.to_value().unwrap()["worker"]["image"].as_str(),
        Some("nginx")
    );
}

/// Was a known round-trip defect: `Stream::document(i)`
/// writes its whole region of the stream's text (header, trailing and
/// footer comments, final line break): the comments its explanations
/// report, whose spans are the stream's, shifted by `span().start` into
/// the document's own text.
#[test]
fn stream_documents_write_their_regions() {
    let src = "# head\n\na: 1 # t\n---\nb: 2 # u\n";
    let stream = Stream::parse(src).unwrap();
    let second = stream.document(1).unwrap();
    assert_eq!(second.to_string(), "---\nb: 2 # u\n");
    let e = second.explain_comments().unwrap();
    assert_eq!(e.len(), 1);
    let span = e[0].span();
    assert_eq!(&src[span.start..span.end], "# u");
    let base = second.span().start;
    let own = second.to_string();
    let (from, to) = (
        span.start.saturating_sub(base),
        span.end.saturating_sub(base),
    );
    assert_eq!(&own[from..to], "# u");
    let first = stream.document(0).unwrap();
    assert_eq!(first.to_string(), "# head\n\na: 1 # t\n");
    assert_eq!(first.explain_comments().unwrap().len(), 2);
    assert_eq!(first.to_string() + &own, src);
}

/// Was friction: a moved entry still takes the blank
/// lines before its leading comments with it (rule 8); the blank-line
/// setters tidy the result.
#[test]
fn move_entry_carries_its_blank_line() {
    let src = "jobs:\n  # a\n  a: 1\n\n  # b\n  b: 2\n";
    let mut doc = Document::parse(src).unwrap();
    doc.get_mut("jobs").unwrap().move_entry(1, 0).unwrap();
    assert_eq!(doc.to_string(), "jobs:\n\n  # b\n  b: 2\n  # a\n  a: 1\n");
    let ids = doc.comment_ids();
    doc.set_comment_blank_lines(ids[0], 0).unwrap();
    doc.set_comment_blank_lines(ids[1], 1).unwrap();
    assert_eq!(doc.to_string(), "jobs:\n  # b\n  b: 2\n\n  # a\n  a: 1\n");
}

/// Style inference matches the
/// neighbours: a null `get_path_or_create` creates is written as its
/// neighbour writes null; `null` when there is no such neighbour.
#[test]
fn new_null_entry_follows_its_neighbours() {
    let src = "on:\n  push:\n  pull_request:\n";
    let mut doc = Document::parse(src).unwrap();
    doc.get_path_or_create(&path!["on", "workflow_dispatch"])
        .unwrap();
    assert_eq!(
        doc.to_string(),
        "on:\n  push:\n  pull_request:\n  workflow_dispatch:\n"
    );
    let mut doc = Document::parse("a: ~\nb: 1\nc: ~\n").unwrap();
    doc.get_path_or_create(&path!["d"]).unwrap();
    assert_eq!(doc.to_string(), "a: ~\nb: 1\nc: ~\nd: ~\n");
    let mut doc = Document::parse("- x\n-\n").unwrap();
    doc.get_path_or_create(&path![2]).unwrap();
    assert_eq!(doc.to_string(), "- x\n-\n-\n");
    // No null neighbour: `null`, as before; nested creations too.
    let mut doc = Document::parse("a: 1\n").unwrap();
    doc.get_path_or_create(&path!["b"]).unwrap();
    doc.get_path_or_create(&path!["c", "d"]).unwrap();
    assert_eq!(doc.to_string(), "a: 1\nb: null\nc:\n  d: null\n");
}

/// Friction (documented on `attach_comment`): an explicit owner relabels a
/// comment for reads, but which comments a removal takes is still decided
/// by the rulebook. Re-owning a commented-out block to the list's footer
/// does not keep it when the item below it is removed; the explicit owner
/// lapses with it. `remove_keep_comments` is the tool for that.
#[test]
fn explicit_owner_does_not_keep_a_comment_from_leaving_with_its_rulebook_owner() {
    use oneyaml::CommentOwner;
    let src = "ports:\n  - name: http\n  # - name: https\n  - name: metrics\nenv: []\n";
    let mut doc = Document::parse(src).unwrap();
    let ports = doc.id_of(&path!["ports"]).unwrap();
    let commented = doc.comment_ids()[0];
    doc.attach_comment(commented, CommentOwner::Footer(ports))
        .unwrap();
    let footer = doc.comments(ports).unwrap();
    assert_eq!(footer.footer().len(), 1);
    assert!(footer.footer()[0].is_explicit());
    let metrics = doc.id_of(&path!["ports", 1]).unwrap();
    let removed = doc.remove_node(metrics).unwrap();
    assert_eq!(removed.comments(), ["# - name: https"]);
    assert_eq!(doc.to_string(), "ports:\n  - name: http\nenv: []\n");
    // What a user wanted: `remove_keep_comments` (rule 6 footer).
    let mut doc = Document::parse(src).unwrap();
    let metrics = doc.id_of(&path!["ports", 1]).unwrap();
    let removed = doc.remove_keep_comments(metrics).unwrap();
    assert!(removed.comments().is_empty());
    assert_eq!(
        doc.to_string(),
        "ports:\n  - name: http\n  # - name: https\nenv: []\n"
    );
    let e = doc.explain_comments().unwrap();
    let ports = doc.id_of(&path!["ports"]).unwrap();
    assert_eq!(e[0].owner(), CommentOwner::Footer(ports));
    // `detach_comment` on a comment without an explicit owner: `None`.
    assert_eq!(doc.detach_comment(e[0].id()).unwrap(), None);
}
