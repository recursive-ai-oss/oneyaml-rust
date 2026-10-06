//! Well-formedness is judged in the edit's real context.
//!
//! Each reproduction here was once accepted
//! although the edited text re-parses **with errors** while the tree has
//! none. They must be refused now, leaving the tree unchanged, and the
//! well-formed variants next to them must still be accepted and agree
//! with their re-parse (error sets included).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod edit_support;

use edit_support::{invariants, nth, reparses};
use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxKind as K, SyntaxTree, TreeEditError};

/// The three line breaks (§5.4 `b-break`; the tests whose
/// outcome depends on line breaks run over each).
const EOLS: [&str; 3] = ["\n", "\r\n", "\r"];

/// The edited tree agrees with its own text: the same structure and the
/// same diagnostics (error set) as its re-parse.
fn agrees(tree: &SyntaxTree) {
    invariants(tree);
    reparses(tree, tree.settings());
}

/// `edit` is refused as not well-formed (or as a limit), and the tree is
/// exactly as it was.
fn refused(
    tree: &mut SyntaxTree,
    edit: impl FnOnce(&mut SyntaxTree) -> Result<(), TreeEditError>,
    what: &str,
) -> TreeEditError {
    let text = tree.emit();
    let ids: Vec<NodeId> = tree.root().descendants().map(|n| n.id()).collect();
    let err = edit(tree).expect_err(what);
    assert!(
        matches!(
            err,
            TreeEditError::NotWellFormed | TreeEditError::Limit { .. }
        ),
        "{what}: {err:?}"
    );
    assert_eq!(tree.emit(), text, "{what}: text changed");
    for id in ids {
        assert!(tree.get(id).is_ok(), "{what}: a handle went stale");
    }
    // The would-be text really re-parses with errors or differently; the
    // tree itself still agrees with its text.
    agrees(tree);
    err
}

/// The token of `kind` whose text is `text`.
fn token(tree: &SyntaxTree, kind: K, text: &str) -> NodeId {
    tree.root()
        .descendants()
        .find(|n| n.kind() == kind && n.text() == text)
        .unwrap_or_else(|| panic!("no {kind:?} {text:?}"))
        .id()
}

// ============================== flow continuation lines keep block indentation

/// `k: [a,\n b]`: the flow line needs indentation past the block mapping's
/// (§7, n+1). Removing the white space before `b` was accepted (Y0005).
#[test]
fn flow_continuation_lines_keep_block_indentation() {
    for eol in EOLS {
        flow_continuation_lines_keep_block_indentation_with(eol);
    }
}

fn flow_continuation_lines_keep_block_indentation_with(eol: &str) {
    let n = |s: &str| s.replace('\n', eol);
    let mut tree = SyntaxTree::parse(n("k: [a,\n b]\n"));
    let ws = token(&tree, K::Whitespace, " ");
    // The first " " is after `:`; the one before `b` is the second.
    let ws2 = nth(&tree, K::Whitespace, 1).id();
    assert_ne!(ws, ws2);
    refused(
        &mut tree,
        |t| t.remove(ws2).map(|_| ()),
        "remove indentation",
    );
    // More indentation is fine.
    let ws2 = nth(&tree, K::Whitespace, 1).id();
    tree.replace_token(ws2, "   ").unwrap();
    assert_eq!(tree.emit(), n("k: [a,\n   b]\n"));
    agrees(&tree);
}

/// `a:\n  k: [x,\n    y]`: the flow line inside the nested mapping needs
/// more than 2 spaces; one space was accepted (Y0005).
#[test]
fn nested_flow_lines_keep_the_inner_block_indentation() {
    for eol in EOLS {
        nested_flow_lines_keep_the_inner_block_indentation_with(eol);
    }
}

fn nested_flow_lines_keep_the_inner_block_indentation_with(eol: &str) {
    let n = |s: &str| s.replace('\n', eol);
    let mut tree = SyntaxTree::parse(n("a:\n  k: [x,\n    y]\n"));
    let ws = token(&tree, K::Whitespace, "    ");
    refused(
        &mut tree,
        |t| t.replace_token(ws, " ").map(|_| ()),
        "one space",
    );
    refused(
        &mut tree,
        |t| t.replace_token(ws, "  ").map(|_| ()),
        "two spaces (the mapping's own column)",
    );
    tree.replace_token(ws, "   ").unwrap();
    assert_eq!(tree.emit(), n("a:\n  k: [x,\n   y]\n"));
    agrees(&tree);
}

/// `a:\n  k: ["x\n    y"]`: a multi-line quoted scalar in flow inside
/// block needs its continuation lines indented (Y0011).
#[test]
fn quoted_continuation_lines_keep_block_indentation() {
    for eol in EOLS {
        quoted_continuation_lines_keep_block_indentation_with(eol);
    }
}

fn quoted_continuation_lines_keep_block_indentation_with(eol: &str) {
    let n = |s: &str| s.replace('\n', eol);
    let mut tree = SyntaxTree::parse(n("a:\n  k: [\"x\n    y\"]\n"));
    let dq = tree
        .root()
        .descendants()
        .find(|n| n.kind() == K::DoubleQuotedScalar)
        .unwrap()
        .id();
    refused(
        &mut tree,
        |t| t.replace_token(dq, &n("\"x\ny\"")).map(|_| ()),
        "dq at column 0",
    );
    tree.replace_token(dq, &n("\"x\n   y\"")).unwrap();
    agrees(&tree);
}

/// `k:\n  - ['a\n     b']`: the same for a single-quoted scalar inside a
/// sequence entry (Y0011).
#[test]
fn single_quoted_continuation_lines_keep_block_indentation() {
    for eol in EOLS {
        single_quoted_continuation_lines_keep_block_indentation_with(eol);
    }
}

fn single_quoted_continuation_lines_keep_block_indentation_with(eol: &str) {
    let n = |s: &str| s.replace('\n', eol);
    let mut tree = SyntaxTree::parse(n("k:\n  - ['a\n     b']\n"));
    let sq = tree
        .root()
        .descendants()
        .find(|n| n.kind() == K::SingleQuotedScalar)
        .unwrap()
        .id();
    refused(
        &mut tree,
        |t| t.replace_token(sq, &n("'a\nb'")).map(|_| ()),
        "sq at column 0",
    );
    refused(
        &mut tree,
        |t| t.replace_token(sq, &n("'a\n  b'")).map(|_| ()),
        "sq at the sequence's column",
    );
    tree.replace_token(sq, &n("'a\n   b'")).unwrap();
    agrees(&tree);
}

/// `[a, b]: v`: a block implicit key is one line (§8.2.2); a line break in
/// the flow key was accepted (Y0007).
#[test]
fn implicit_flow_keys_stay_on_one_line() {
    let mut tree = SyntaxTree::parse("[a, b]: v\n");
    let ws = token(&tree, K::Whitespace, " ");
    let nl = Fragment::parse("\n").unwrap();
    refused(
        &mut tree,
        |t| t.insert_before(ws, nl).map(|_| ()),
        "line break in the key",
    );
    // Also nested: the inner sequence is part of the outer key.
    let mut tree = SyntaxTree::parse("[[a, b]]: v\n");
    let comma = nth(&tree, K::FlowEntry, 0).id();
    let nl = Fragment::parse("\n").unwrap();
    refused(
        &mut tree,
        |t| t.insert_after(comma, nl).map(|_| ()),
        "line break after a comma of a nested key",
    );
    // And in a flow pair's key inside a flow sequence.
    let mut tree = SyntaxTree::parse("- [[a, b]: v]\n");
    let comma = nth(&tree, K::FlowEntry, 0).id();
    let nl = Fragment::parse("\n").unwrap();
    refused(
        &mut tree,
        |t| t.insert_after(comma, nl).map(|_| ()),
        "line break in a flow pair's key",
    );
    // A one-line edit of the key is fine.
    let mut tree = SyntaxTree::parse("[a, b]: v\nw: 1\n");
    let a = token(&tree, K::PlainScalar, "a");
    tree.replace_token(a, "aa").unwrap();
    assert_eq!(tree.emit(), "[aa, b]: v\nw: 1\n");
    agrees(&tree);
}

/// `[a, b]: v`: an implicit key is at most 1024 characters (§8.2.2); a
/// 1100-character key was accepted (Y0025).
#[test]
fn implicit_flow_keys_stay_within_1024_characters() {
    let mut tree = SyntaxTree::parse("[a, b]: v\n");
    let a = token(&tree, K::PlainScalar, "a");
    let long = "x".repeat(1100);
    refused(
        &mut tree,
        |t| t.replace_token(a, &long).map(|_| ()),
        "1100-character key",
    );
    let ok = "x".repeat(1000);
    tree.replace_token(a, &ok).unwrap();
    agrees(&tree);
    // A plain scalar key too.
    let mut tree = SyntaxTree::parse("a: v\n");
    let a = token(&tree, K::PlainScalar, "a");
    refused(
        &mut tree,
        |t| t.replace_token(a, &long).map(|_| ()),
        "1100-character plain key",
    );
}

// ===================================================== edits respect max depth

/// `max_depth(4)`, `[[[x]]]`: replacing `x` by `[[y]]` gives depth 5; it
/// was accepted, and the re-parse reports Y2000.
#[test]
fn edits_respect_max_depth() {
    let settings = Settings::new().limits(Limits::new().max_depth(4));
    let mut tree = SyntaxTree::parse_with("[[[x]]]\n", &settings);
    assert!(!tree.has_errors());
    let x = nth(&tree, K::Scalar, 0).id();
    let deep = Fragment::parse_with("[[y]]", &settings).unwrap();
    let err = refused(
        &mut tree,
        |t| t.replace(x, deep).map(|_| ()),
        "depth 5 under max_depth(4)",
    );
    assert!(
        matches!(err, TreeEditError::Limit { code } if code.to_string() == "Y2000"),
        "{err:?}"
    );
    // Depth 4 is fine.
    let x = nth(&tree, K::Scalar, 0).id();
    let _ = tree
        .replace(x, Fragment::parse_with("[y]", &settings).unwrap())
        .unwrap();
    agrees(&tree);
    // Block nesting counts too, from the tree's root (not the window's).
    let mut tree = SyntaxTree::parse_with("a:\n  b:\n    c: 1\n", &settings);
    let one = nth(&tree, K::Scalar, 3).id();
    let err = refused(
        &mut tree,
        |t| {
            t.replace(one, Fragment::parse_with("[[z]]", &settings).unwrap())
                .map(|_| ())
        },
        "block depth 3 + flow depth 2",
    );
    assert!(matches!(err, TreeEditError::Limit { .. }), "{err:?}");
    let _ = tree
        .replace(one, Fragment::parse_with("[z]", &settings).unwrap())
        .unwrap();
    agrees(&tree);
}

/// `max_document_size`: an edit may not grow the text past it (the
/// re-parse would skip the whole input, Y2002).
#[test]
fn edits_respect_max_document_size() {
    let settings = Settings::new().limits(Limits::new().max_document_size(16));
    let mut tree = SyntaxTree::parse_with("a: [1, 2]\n", &settings);
    assert!(!tree.has_errors());
    let one = nth(&tree, K::PlainScalar, 1).id();
    let err = refused(
        &mut tree,
        |t| t.replace_token(one, "123456789").map(|_| ()),
        "19 bytes under max_document_size(16)",
    );
    assert!(
        matches!(err, TreeEditError::Limit { code } if code.to_string() == "Y2002"),
        "{err:?}"
    );
    tree.replace_token(one, "1234").unwrap();
    assert_eq!(tree.emit().len(), 13);
    agrees(&tree);
}

/// `max_scalar_len`: a fragment parsed under other settings may carry a
/// longer scalar than the tree allows.
#[test]
fn edits_respect_max_scalar_len() {
    let settings = Settings::new().limits(Limits::new().max_scalar_len(4));
    let mut tree = SyntaxTree::parse_with("a: [1, 2]\n", &settings);
    let one = nth(&tree, K::Scalar, 1).id();
    let long = Fragment::parse("123456").unwrap();
    let err = refused(
        &mut tree,
        |t| t.replace(one, long).map(|_| ()),
        "6-byte scalar under max_scalar_len(4)",
    );
    assert!(matches!(err, TreeEditError::Limit { .. }), "{err:?}");
}

// ========================================== context around block windows

/// A block collection re-read in place: its entries stay past the
/// enclosing entries, and what follows it keeps its meaning.
#[test]
fn block_windows_read_in_context() {
    for eol in EOLS {
        block_windows_read_in_context_with(eol);
    }
}

fn block_windows_read_in_context_with(eol: &str) {
    let n = |s: &str| s.replace('\n', eol);
    // Appending an entry at the parent's column to a nested mapping
    // would make it the parent's entry.
    let mut tree = SyntaxTree::parse(n("a:\n  b: 1\nc: 2\n"));
    let inner = nth(&tree, K::BlockMapping, 1).id();
    let f = Fragment::parse(&n("\nd: 3"))
        .unwrap()
        .unwrap_root(1)
        .unwrap();
    refused(
        &mut tree,
        |t| t.append_child(inner, f).map(|_| ()),
        "entry at the parent's column",
    );
    let f = Fragment::parse(&n("\n  d: 3"))
        .unwrap()
        .unwrap_root(2)
        .unwrap();
    let _ = tree.append_child(inner, f).unwrap();
    assert_eq!(tree.emit(), n("a:\n  b: 1\n  d: 3\nc: 2\n"));
    agrees(&tree);
    // A keep-chomped block scalar takes the blank lines after it.
    let mut tree = SyntaxTree::parse(n("a: |+\n  x\nb: 1\n"));
    let b = nth(&tree, K::Scalar, 2).id();
    let blank = Fragment::parse(&n("\n")).unwrap();
    refused(
        &mut tree,
        |t| t.insert_before(b, blank).map(|_| ()),
        "blank line into a keep-chomped scalar",
    );
    // An indented comment line after a literal scalar is its content.
    let mut tree = SyntaxTree::parse(n("a:\n  k: |\n    x\nb: 1\n"));
    let b = nth(&tree, K::Scalar, 3).id();
    let f = Fragment::parse_with(&n("    # not a comment\n"), tree.settings()).unwrap();
    refused(
        &mut tree,
        |t| t.insert_before(b, f).map(|_| ()),
        "indented comment line into a literal scalar",
    );
    // Less indented, it is a comment.
    let b = nth(&tree, K::Scalar, 3).id();
    let f = Fragment::parse_with(&n("  # a comment\n"), tree.settings()).unwrap();
    let _ = tree.insert_before(b, f).unwrap();
    agrees(&tree);
}

// ======================================================= lenient values

/// The documents of the stream, in order.
fn documents(tree: &SyntaxTree) -> Vec<NodeId> {
    tree.root()
        .children()
        .filter(|n| n.kind() == K::Document)
        .map(|n| n.id())
        .collect()
}

fn is_code(err: TreeEditError, code: &str) -> bool {
    matches!(err, TreeEditError::Limit { code: c } if c.to_string() == code)
}

/// `max_documents` applies to edits, counting the
/// documents a stream-level `replace_range` removes; refused edits (by
/// the limit or by the window) leave the count as it was.
#[test]
fn edits_respect_max_documents() {
    let settings = Settings::new().limits(Limits::new().max_documents(2));
    let mut tree = SyntaxTree::parse_with("--- a\n--- b\n", &settings);
    assert!(!tree.has_errors());
    let docs = documents(&tree);
    // A third document, inserted.
    let err = refused(
        &mut tree,
        |t| {
            t.insert_before(docs[1], Fragment::parse("--- c\n").unwrap())
                .map(|_| ())
        },
        "3 documents under max_documents(2)",
    );
    assert!(is_code(err, "Y2003"), "{err:?}");
    // One document replaced by two.
    let err = refused(
        &mut tree,
        |t| {
            t.replace_range(docs[0], docs[0], Fragment::parse("--- p\n--- q\n").unwrap())
                .map(|_| ())
        },
        "one document replaced by two",
    );
    assert!(is_code(err, "Y2003"), "{err:?}");
    // Two replaced by two: the removed ones are counted out. (The last
    // line break is the stream's.)
    let r = tree
        .replace_range(docs[0], docs[1], Fragment::parse("--- x\n--- y").unwrap())
        .unwrap();
    let removed_docs = r.removed.root_kinds().filter(|k| *k == K::Document).count();
    assert_eq!(removed_docs, 2);
    assert_eq!(tree.emit(), "--- x\n--- y\n");
    agrees(&tree);
    // Removed, a document makes room for another.
    let docs = documents(&tree);
    let _ = tree.remove(docs[0]).unwrap();
    let _ = tree
        .insert_before(docs[1], Fragment::parse("--- z\n").unwrap())
        .unwrap();
    assert_eq!(tree.emit(), "\n--- z\n--- y\n");
    agrees(&tree);
    let docs = documents(&tree);
    let err = refused(
        &mut tree,
        |t| {
            t.insert_before(docs[1], Fragment::parse("--- w\n").unwrap())
                .map(|_| ())
        },
        "a third document after a remove and an insert",
    );
    assert!(is_code(err, "Y2003"), "{err:?}");

    // Under max_documents(3): a document refused by the window (not
    // well-formed) leaves the count as it was, so one more is accepted,
    // and the next is refused by the limit.
    let settings = Settings::new().limits(Limits::new().max_documents(3));
    let mut tree = SyntaxTree::parse_with("--- a\n--- b\n", &settings);
    let docs = documents(&tree);
    let err = refused(
        &mut tree,
        |t| {
            t.insert_before(docs[1], Fragment::parse("--- c").unwrap())
                .map(|_| ())
        },
        "a document running into the next",
    );
    assert_eq!(err, TreeEditError::NotWellFormed);
    let _ = tree
        .insert_before(docs[1], Fragment::parse("--- c\n").unwrap())
        .unwrap();
    assert_eq!(tree.emit(), "--- a\n--- c\n--- b\n");
    agrees(&tree);
    let docs = documents(&tree);
    let err = refused(
        &mut tree,
        |t| {
            t.insert_before(docs[0], Fragment::parse("--- d\n").unwrap())
                .map(|_| ())
        },
        "4 documents under max_documents(3)",
    );
    assert!(is_code(err, "Y2003"), "{err:?}");
    // Replacing all three by one, then two more fit.
    let docs = documents(&tree);
    let _ = tree
        .replace_range(docs[0], docs[2], Fragment::parse("--- e").unwrap())
        .unwrap();
    assert_eq!(tree.emit(), "--- e\n");
    let docs = documents(&tree);
    let _ = tree
        .insert_before(docs[0], Fragment::parse("--- f\n--- g\n").unwrap())
        .unwrap();
    assert_eq!(tree.emit(), "--- f\n--- g\n--- e\n");
    agrees(&tree);
    // A clone keeps the count.
    let mut copy = tree.clone();
    let docs = documents(&copy);
    let err = refused(
        &mut copy,
        |t| {
            t.insert_before(docs[2], Fragment::parse("--- h\n").unwrap())
                .map(|_| ())
        },
        "4 documents in a clone",
    );
    assert!(is_code(err, "Y2003"), "{err:?}");
}

/// The parser counts a flow pair toward `max_depth`
/// (it starts a single-pair mapping), and so do edits: at the limit a
/// token edit inside a pair is accepted, one more level is refused, and a
/// pair inserted where it fits is accepted.
#[test]
fn flow_pairs_count_toward_max_depth_as_in_the_parser() {
    // `[a: [x]]`: sequence, pair, sequence: depth 3.
    let at = |d: usize| Settings::new().limits(Limits::new().max_depth(d));
    assert!(SyntaxTree::parse_with("[a: [x]]\n", &at(2)).has_errors());
    let mut tree = SyntaxTree::parse_with("[a: [x]]\n", &at(3));
    assert!(!tree.has_errors());
    let x = token(&tree, K::PlainScalar, "x");
    let _ = tree.replace_token(x, "y").unwrap();
    agrees(&tree);
    let y = nth(&tree, K::Scalar, 1).id();
    let err = refused(
        &mut tree,
        |t| t.replace(y, Fragment::parse("[z]").unwrap()).map(|_| ()),
        "depth 4 under max_depth(3)",
    );
    assert!(is_code(err, "Y2000"), "{err:?}");
    // A pair inserted into `[[x]]` (depth 2) needs depth 3.
    let pair = {
        let t = SyntaxTree::parse("[k: v]");
        let p = t
            .root()
            .descendants()
            .find(|n| n.kind() == K::FlowPair)
            .unwrap()
            .id();
        t.extract(p, p).unwrap()
    };
    let mut tree = SyntaxTree::parse_with("[[x]]\n", &at(2));
    let x = nth(&tree, K::Scalar, 0).id();
    let err = refused(
        &mut tree,
        |t| t.replace(x, pair.clone()).map(|_| ()),
        "a pair at depth 3 under max_depth(2)",
    );
    assert!(is_code(err, "Y2000"), "{err:?}");
    let mut tree = SyntaxTree::parse_with("[[x]]\n", &at(3));
    let x = nth(&tree, K::Scalar, 0).id();
    let _ = tree.replace(x, pair).unwrap();
    assert_eq!(tree.emit(), "[[k: v]]\n");
    agrees(&tree);
}

/// With `Y0028` warnings all over the document, the check for
/// labels that would move across the edit still sees the one next to it:
/// an edit between a lenient scalar and the element its warning is
/// attached to agrees with its re-parse (or is refused, unchanged), and
/// edits elsewhere are accepted.
#[test]
fn lenient_warnings_next_to_an_edit_are_seen() {
    for eol in EOLS {
        lenient_warnings_next_to_an_edit_are_seen_with(eol);
    }
}

fn lenient_warnings_next_to_an_edit_are_seen_with(eol: &str) {
    let n = |s: &str| s.replace('\n', eol);
    let settings = Settings::new().lenient_inline_colons(true);
    let src = &n("a: b: c\nd: e: f\ng: 1\n");
    let tree = SyntaxTree::parse_with(src, &settings);
    assert_eq!(tree.diagnostics().len(), 2);
    assert!(!tree.has_errors());
    // Right after the scalar `e: f` (the warning's element follows it).
    for text in [" ", " # x"] {
        let mut t = tree.clone();
        let ef = t
            .root()
            .descendants()
            .find(|n| n.kind() == K::Scalar && n.text() == "e: f")
            .unwrap()
            .id();
        let before = t.emit();
        match t.insert_after(ef, Fragment::parse_with(text, &settings).unwrap()) {
            Ok(_) => agrees(&t),
            Err(e) => {
                assert_eq!(e, TreeEditError::NotWellFormed);
                assert_eq!(t.emit(), before);
                agrees(&t);
            }
        }
    }
    // Elsewhere: accepted, the warnings moved with their text.
    let mut t = tree.clone();
    let one = token(&t, K::PlainScalar, "1");
    let _ = t.replace_token(one, "22").unwrap();
    agrees(&t);
    let mut t = tree.clone();
    let a = token(&t, K::PlainScalar, "a");
    let _ = t.replace_token(a, "aaa").unwrap();
    agrees(&t);
    let spans: Vec<_> = t.diagnostics().map(|d| d.primary.span).collect();
    let again: Vec<_> = SyntaxTree::parse_with(t.emit(), &settings)
        .diagnostics()
        .map(|d| d.primary.span)
        .collect();
    assert_eq!(spans, again);
}

/// Found by the lenient rounds of the edit fuzz: a `Y0028` warning is attached
/// to the element after its scalar. Removing the text's last line break, which
/// carries it, would leave a warning after the last element in the re-parse
/// while the tree dropped it: refused. Without the warning the same edit is
/// accepted.
#[test]
fn a_warning_at_the_end_of_the_text_is_not_dropped() {
    for eol in EOLS {
        a_warning_at_the_end_of_the_text_is_not_dropped_with(eol);
    }
}

fn a_warning_at_the_end_of_the_text_is_not_dropped_with(eol: &str) {
    let n = |s: &str| s.replace('\n', eol);
    let lenient = Settings::new().lenient_inline_colons(true);
    let mut tree = SyntaxTree::parse_with(n("a: 1\nk: x: z\n"), &lenient);
    assert_eq!(tree.diagnostics().len(), 1);
    let last = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == K::LineBreak)
        .last()
        .unwrap()
        .id();
    let err = refused(
        &mut tree,
        |t| t.remove(last).map(|_| ()),
        "the line break carrying the warning",
    );
    assert_eq!(err, TreeEditError::NotWellFormed);
    assert_eq!(tree.diagnostics().len(), 1);
    // Without a warning, the last line break may go.
    let mut tree = SyntaxTree::parse(n("a: 1\nk: x\n"));
    let last = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == K::LineBreak)
        .last()
        .unwrap()
        .id();
    let _ = tree.remove(last).unwrap();
    assert_eq!(tree.emit(), n("a: 1\nk: x"));
    agrees(&tree);
}
