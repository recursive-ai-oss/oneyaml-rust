#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use oneyaml_core::code::{INTERNAL_ERROR, INVALID_EVENT_STREAM, TREE_CAPACITY_EXCEEDED};
use oneyaml_core::{Settings, Span};

use super::build::{Builder, Origin};
use super::*;
use crate::event::{EventKind, TriviaKind};
use crate::parse;

/// Builds, then checks the tree against its stream (consistency check (1)).
fn checked(src: &str) -> SyntaxTree {
    let tree = SyntaxTree::parse(src);
    let events = parse(src);
    if let Err(e) = check_tree(&events, &tree) {
        panic!("{src:?}: {e}\n{tree:?}");
    }
    tree
}

/// `kind` of each child of `node`.
fn kinds(node: Node<'_>) -> Vec<SyntaxKind> {
    node.children().map(Node::kind).collect()
}

fn find(tree: &SyntaxTree, kind: SyntaxKind) -> Node<'_> {
    tree.root()
        .descendants()
        .find(|n| n.kind() == kind)
        .unwrap()
}

#[test]
fn block_mapping_shape() {
    use SyntaxKind as K;
    let tree = checked("# head\na: [1, 2] # c\n");
    let root = tree.root();
    assert_eq!(root.kind(), K::Stream);
    assert_eq!(root.span(), Span::new(0, 21));
    // Leading comments lie before the implicit document start (C4).
    assert_eq!(
        kinds(root),
        [
            K::Comment,
            K::LineBreak,
            K::Document,
            K::Whitespace,
            K::Comment,
            K::LineBreak
        ]
    );
    let doc = root.children().nth(2).unwrap();
    assert_eq!(doc.text(), "a: [1, 2]");
    let map = doc.first_child().unwrap();
    assert_eq!(
        kinds(map),
        [K::Scalar, K::MappingValue, K::Whitespace, K::FlowSequence]
    );
    let seq = map.last_child().unwrap();
    assert_eq!(
        kinds(seq),
        [
            K::FlowSequenceOpen,
            K::Scalar,
            K::FlowEntry,
            K::Whitespace,
            K::Scalar,
            K::FlowSequenceClose
        ]
    );
    assert_eq!(seq.text(), "[1, 2]");
    let one = seq.children().nth(1).unwrap();
    assert_eq!(one.value().as_deref(), Some("1"));
    assert_eq!(one.first_child().unwrap().kind(), K::PlainScalar);
    assert!(!tree.has_errors());
}

#[test]
fn properties_and_headers_belong_to_their_node() {
    use SyntaxKind as K;
    let tree = checked("&a !!str x\n");
    let scalar = find(&tree, K::Scalar);
    assert_eq!(scalar.text(), "&a !!str x");
    assert_eq!(
        kinds(scalar),
        [
            K::Anchor,
            K::Whitespace,
            K::Tag,
            K::Whitespace,
            K::PlainScalar
        ]
    );
    assert_eq!(scalar.anchor().unwrap().name, "a");
    assert_eq!(scalar.tag().unwrap().resolved, "tag:yaml.org,2002:str");
    assert_eq!(scalar.value().as_deref(), Some("x"));

    // A block collection's properties, on the line before it.
    let tree = checked("&m\n&k k: v\n");
    let map = find(&tree, K::BlockMapping);
    assert_eq!(kinds(map)[..3], [K::Anchor, K::LineBreak, K::Scalar]);
    assert_eq!(map.anchor().unwrap().name, "m");
    let key = map.children().nth(2).unwrap();
    assert_eq!(key.anchor().unwrap().name, "k");
    assert_eq!(key.text(), "&k k");

    // A block scalar's header, comment, and break are its own.
    let tree = checked("k: !t |- # c\n  text\n\n");
    let block = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == K::Scalar)
        .nth(1)
        .unwrap();
    assert_eq!(
        kinds(block),
        [
            K::Tag,
            K::Whitespace,
            K::BlockScalarHeader,
            K::Whitespace,
            K::Comment,
            K::LineBreak,
            K::LiteralScalar
        ]
    );
    assert_eq!(block.value().as_deref(), Some("text"));
    assert_eq!(block.block_header().unwrap().content_indent, 2);
    assert!(block.anchor().is_none());
    // Trailing empty lines of a strip scalar are trivia after it (C5).
    assert_eq!(tree.root().last_child().unwrap().kind(), K::LineBreak);
}

#[test]
fn directives_belong_to_their_document() {
    use SyntaxKind as K;
    let tree = checked("# c\n%TAG !e! tag:e,2000:\n--- !e!x a\n...\n");
    let root = tree.root();
    assert_eq!(
        kinds(root),
        [K::Comment, K::LineBreak, K::Document, K::LineBreak]
    );
    let doc = root.children().nth(2).unwrap();
    assert_eq!(
        kinds(doc),
        [
            K::TagDirective,
            K::LineBreak,
            K::DocumentStartMarker,
            K::Whitespace,
            K::Scalar,
            K::LineBreak,
            K::DocumentEndMarker
        ]
    );
    let scalar = find(&tree, K::Scalar);
    assert_eq!(scalar.tag().unwrap().resolved, "tag:e,2000:x");
}

#[test]
fn aliases_and_empty_nodes() {
    use SyntaxKind as K;
    let tree = checked("- &x\n- *x\n-\n");
    let seq = find(&tree, K::BlockSequence);
    let nodes: Vec<Node<'_>> = seq.children().filter(|n| n.kind().is_yaml_node()).collect();
    assert_eq!(nodes.len(), 3);
    assert_eq!(nodes[0].text(), "&x");
    assert_eq!(nodes[0].value().as_deref(), Some(""));
    assert_eq!(nodes[1].kind(), K::Alias);
    assert_eq!(nodes[1].alias_name(), Some("x"));
    assert_eq!(nodes[1].value(), None);
    // An empty node is zero-width, with a zero-width text token.
    assert_eq!(nodes[2].span(), Span::empty(11));
    assert_eq!(kinds(nodes[2]), [K::PlainScalar]);
    assert_eq!(nodes[2].text(), "");
}

#[test]
fn errors_become_error_nodes() {
    use SyntaxKind as K;
    for src in [
        "a: b: c\nd\n",
        "[a\n",
        "- a\n b: c\n--- x\n",
        "&a\n%YAML 1.2\n",
        "k: |0\n x\n",
    ] {
        let tree = checked(src);
        assert!(tree.has_errors(), "{src:?}");
        let error = find(&tree, K::Error);
        assert_eq!(kinds(error), [K::ErrorText]);
        assert!(error.diagnostic().is_some(), "{src:?}");
        assert_eq!(tree.emit(), src);
    }
    let tree = checked("a: b: c\nd\n");
    assert!(tree.root().descendants().any(|n| n.kind() == K::Skipped));
    assert!(find(&tree, K::Scalar).diagnostic().is_none());
}

#[test]
fn navigation_is_consistent() {
    let inputs = [
        "a:\n  - b\n  -\n    c: d # e\n  - &y !!str\nf: |\n  g\n",
        "%YAML 1.2\n--- !e &x\nk: 'v''w'\n? - \"q\\t\n  r\"\n: *x\n...\n",
        "[a, {b: c}, [d: e], ? f]\n",
        "a: b: c\n- x\n",
        "",
        "\u{feff}# only\n",
    ];
    for src in inputs {
        let tree = checked(src);
        let mut entered = 0;
        let mut depth = 0isize;
        for step in tree.root().walk() {
            match step {
                WalkEvent::Enter(_) => {
                    entered += 1;
                    depth += 1;
                }
                WalkEvent::Leave(_) => depth -= 1,
            }
            assert!(depth >= 0);
        }
        assert_eq!(depth, 0);
        assert_eq!(entered, tree.len());
        for node in tree.root().descendants() {
            let children: Vec<Node<'_>> = node.children().collect();
            assert_eq!(node.first_child(), children.first().copied());
            assert_eq!(node.last_child(), children.last().copied());
            let mut at = node.span().start;
            for pair in children.windows(2) {
                assert_eq!(pair[0].next_sibling(), Some(pair[1]));
                assert_eq!(pair[1].prev_sibling(), Some(pair[0]));
            }
            for child in &children {
                assert_eq!(child.parent(), Some(node));
                assert_eq!(child.span().start, at, "{src:?} {child:?}");
                at = child.span().end;
            }
            if !children.is_empty() {
                assert_eq!(at, node.span().end);
            }
            if let Some(first) = children.first() {
                assert_eq!(first.prev_sibling(), None);
            }
            if let Some(last) = children.last() {
                assert_eq!(last.next_sibling(), None);
            }
            assert_eq!(
                node.ancestors()
                    .last()
                    .map(Node::kind)
                    .unwrap_or(node.kind()),
                SyntaxKind::Stream
            );
        }
        assert_eq!(tree.root().span(), Span::new(0, src.len()));
        assert_eq!(tree.root().parent(), None);
    }
}

#[test]
fn handles_resolve_only_in_their_tree() {
    let tree = SyntaxTree::parse("a: b\n");
    let other = SyntaxTree::parse("a: b\n");
    let node = find(&tree, SyntaxKind::Scalar);
    let id = node.id();
    assert_eq!(tree.get(id), Ok(node));
    assert_eq!(other.get(id), Err(StaleNode { id }));
    let copy = tree.clone();
    assert_eq!(copy, tree);
    assert!(copy.get(id).is_err());
    // Out of range.
    let far = NodeId { index: 999, ..id };
    assert_eq!(tree.get(far), Err(StaleNode { id: far }));
    // A retired slot (the edit layer bumps generations).
    let mut edited = tree.clone();
    let id = edited.root().first_child().unwrap().id();
    edited.generations.insert(id.index, 1);
    assert!(edited.get(id).is_err());
    assert_eq!(
        edited
            .get(edited.root().first_child().unwrap().id())
            .unwrap()
            .kind(),
        SyntaxKind::Document
    );
    assert!(!StaleNode { id }.to_string().is_empty());
}

#[test]
fn equality_and_hash_ignore_the_stamp() {
    use std::hash::{BuildHasher, RandomState};
    let a = SyntaxTree::parse("a: [b]\n");
    let b = SyntaxTree::parse(String::from("a: [b]\n"));
    let c = SyntaxTree::parse("a: [c]\n");
    let s = RandomState::new();
    assert_eq!(a, b);
    assert_eq!(s.hash_one(&a), s.hash_one(&b));
    assert_ne!(a, c);
    let from_events = SyntaxTree::from_events(parse("a: [b]\n").sourced());
    assert_eq!(from_events, a);
    let owned = SyntaxTree::from_events(parse("a: [b]\n"));
    assert_eq!(owned, a);
    let debug = format!("{a:?}");
    assert!(debug.contains("FlowSequence@3..6"), "{debug}");
}

#[test]
fn settings_reach_the_parser() {
    let src = "title: The farm: a story\n";
    assert!(SyntaxTree::parse(src).has_errors());
    let lenient = Settings::new().lenient_inline_colons(true);
    let tree = SyntaxTree::parse_with(src, &lenient);
    assert!(!tree.has_errors());
    assert_eq!(tree.diagnostics().len(), 1);
    check_tree(&crate::parse_with(src, &lenient), &tree).unwrap();
}

#[test]
fn empty_input_and_empty_stream() {
    let tree = checked("");
    assert_eq!(tree.len(), 1);
    assert!(!tree.is_empty());
    assert_eq!(tree.emit(), "");
    let none = SyntaxTree::from_events(Vec::<crate::event::Sourced<'_>>::new());
    assert_eq!(none.len(), 1);
    assert_eq!(none.root().span(), Span::empty(0));
    assert_eq!(none.diagnostics().len(), 0);
}

#[test]
fn malformed_streams_build_without_panicking() {
    let src = "a: [b, c]\n- d\n";
    let events = parse(src);
    let n = events.len();
    // Drop each event in turn, and reverse the stream.
    for skip in 0..n {
        let tree = SyntaxTree::from_events(
            events
                .sourced()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, e)| e),
        );
        let _ = tree.emit();
        let _ = format!("{tree:?}");
        if tree.emit() != src {
            assert!(
                tree.diagnostics().any(|d| d.code == INVALID_EVENT_STREAM),
                "{skip}"
            );
        }
    }
    let tree = SyntaxTree::from_events(events.sourced().rev());
    assert!(tree.diagnostics().any(|d| d.code == INVALID_EVENT_STREAM));
    let _ = tree.emit();
    // A stream missing its tail: the rest is covered as skipped bytes.
    let tree = SyntaxTree::from_events(events.sourced().take(3));
    assert_eq!(tree.emit(), src);
    assert!(tree.diagnostics().any(|d| d.code == INVALID_EVENT_STREAM));
    assert_eq!(
        tree.root().last_child().unwrap().kind(),
        SyntaxKind::Skipped
    );
}

/// Y9001 for a caller's invalid stream (API misuse, help says so), Y9000
/// when the parser's own stream is broken (a bug; driven directly, since
/// no input breaks it), and nothing for a valid stream.
#[test]
fn contract_violations_are_coded_by_origin() {
    let src = "a: [b, c]\n";
    let events = parse(src);
    let clean = SyntaxTree::from_events(events.sourced());
    assert_eq!(clean.diagnostics().len(), 0);
    let bad = SyntaxTree::from_events(events.sourced().skip(2));
    let d: Vec<_> = bad.diagnostics().collect();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].code, INVALID_EVENT_STREAM);
    let help = d[0].help.as_deref().unwrap();
    assert!(help.contains("input stream was invalid"), "{help}");
    assert!(help.contains("not a bug"), "{help}");
    for origin in [Origin::Parser, Origin::Caller] {
        let mut b = Builder::new(src.len(), origin);
        for e in events.iter().skip(2) {
            b.push(e.clone());
        }
        let tree = SyntaxTree::from_parts(src.to_owned(), b.finish(), Settings::new());
        let d = tree.diagnostics().next().unwrap();
        match origin {
            Origin::Parser => {
                assert_eq!(d.code, INTERNAL_ERROR);
                assert!(d.help.as_deref().unwrap().contains("bug in oneyaml"));
            }
            Origin::Caller => assert_eq!(d.code, INVALID_EVENT_STREAM),
        }
    }
}

#[test]
fn a_full_arena_skips_the_rest() {
    let src = "a: [b, c]\nd: e\n";
    for cap in 1..40 {
        let mut builder = Builder::with_cap(src.len(), cap, Origin::Parser);
        for e in parse(src).into_iter() {
            builder.push(e.into_event());
        }
        let tree = SyntaxTree::from_parts(src.to_owned(), builder.finish(), Settings::new());
        assert_eq!(tree.emit(), src, "cap {cap}");
        let full = tree.diagnostics().any(|d| d.code == TREE_CAPACITY_EXCEEDED);
        assert_eq!(full, tree.len() > cap, "cap {cap}\n{tree:?}");
        if full {
            let error = tree
                .root()
                .children()
                .find(|n| n.kind() == SyntaxKind::Error)
                .unwrap();
            assert_eq!(error.diagnostic().unwrap().code, TREE_CAPACITY_EXCEEDED);
        }
        for node in tree.root().descendants() {
            let _ = (
                node.span(),
                node.text(),
                node.last_child(),
                node.prev_sibling(),
            );
        }
    }
}

// `1 << 32` needs a 64-bit `usize` (not wasm32).
#[cfg(target_pointer_width = "64")]
#[test]
fn offsets_beyond_four_gib() {
    // Tokens past 2^32 (no real source: only the offset arithmetic).
    const G: usize = 1 << 32;
    let starts = [0, 5, G - 1, G + 7, 3 * G + 2, 3 * G + 3];
    let mut builder = Builder::new(3 * G + 10, Origin::Parser);
    let mut prev = 0;
    for pair in starts
        .windows(2)
        .chain([[3 * G + 3, 3 * G + 10].as_slice()])
    {
        let span = Span::new(pair[0], pair[1]);
        assert_eq!(span.start, prev);
        prev = span.end;
        builder.push(Event::new(EventKind::Trivia(TriviaKind::Skipped), span));
    }
    let tree = SyntaxTree::from_parts(String::new(), builder.finish(), Settings::new());
    assert_eq!(tree.wraps.len(), 3);
    let got: Vec<usize> = (1..tree.len()).map(|i| tree.token_start(i)).collect();
    assert_eq!(got, starts);
}

#[test]
fn check_tree_reports_each_kind_of_difference() {
    let src = "%YAML 1.1\n--- &a [b, *a]\n";
    let events = parse(src);
    assert!(events.iter().any(|e| e.is_warning()));
    let tree = SyntaxTree::parse(src);
    check_tree(&events, &tree).unwrap();
    let fails = |tree: &SyntaxTree| check_tree(&events, tree).unwrap_err().to_string();
    // Another source.
    assert!(fails(&SyntaxTree::parse("x")).contains("another source"));
    // A dropped trivia event: its bytes join the previous token.
    let dropped = |pred: &dyn Fn(&Event<'_>) -> bool| {
        let mut seen = false;
        SyntaxTree::from_events(events.sourced().filter(|e| {
            let hit = !seen && pred(e);
            seen |= hit;
            !hit
        }))
    };
    let msg = fails(&dropped(&|e| {
        matches!(e.kind(), EventKind::Trivia(TriviaKind::Whitespace))
    }));
    assert!(msg.contains("token"), "{msg}");
    // A dropped node: the YAML nodes differ.
    let msg = fails(&dropped(&|e| matches!(e.kind(), EventKind::Alias)));
    assert!(msg.contains("token") || msg.contains("YAML node"), "{msg}");
    // A dropped warning: the diagnostics differ.
    let msg = fails(&dropped(&|e| e.is_warning()));
    assert!(msg.contains("diagnostics"), "{msg}");
    // A reordered stream may not even emit the source.
    let rev = SyntaxTree::from_events(events.sourced().rev());
    assert!(check_tree(&events, &rev).is_err());
    // The nodes differ while tokens agree: an alias read as a scalar.
    let mut swapped: Vec<Event<'_>> = events.to_vec();
    for e in &mut swapped {
        if matches!(e.kind(), EventKind::Alias) {
            *e = Event::new(EventKind::plain(), e.span());
        }
    }
    let swapped = crate::event::EventList::from_parts_unchecked(src, swapped);
    let msg = fails(&SyntaxTree::from_events(swapped.sourced()));
    assert!(msg.contains("YAML node"), "{msg}");
    let extra = crate::event::EventList::from_parts_unchecked(src, events.to_vec());
    let mut more = events.to_vec();
    let at = more.len() - 2;
    more.insert(at, Event::new(EventKind::plain(), Span::empty(src.len())));
    let more = crate::event::EventList::from_parts_unchecked(src, more);
    let msg = check_tree(&extra, &SyntaxTree::from_events(more.sourced()))
        .unwrap_err()
        .to_string();
    assert!(msg.contains("token") || msg.contains("YAML nodes"), "{msg}");
    let msg = check_tree(&more, &tree).unwrap_err().to_string();
    assert!(msg.contains("token"), "{msg}");
    let mut tail = events.to_vec();
    let at = tail.len() - 1;
    tail.insert(at, Event::new(EventKind::plain(), Span::empty(src.len())));
    let tail = crate::event::EventList::from_parts_unchecked(src, tail);
    let msg = check_tree(&tail, &tree).unwrap_err().to_string();
    assert!(msg.contains("no token for"), "{msg}");
    let msg = check_tree(&events, &SyntaxTree::from_events(tail.sourced()))
        .unwrap_err()
        .to_string();
    assert!(msg.contains("extra token"), "{msg}");
}

#[test]
fn iterator_debug_shows_only_the_range() {
    let tree = SyntaxTree::parse("a: [b, c]\nd: {e: f}\n");
    let root = tree.root();
    let children = format!("{:?}", root.children());
    let descendants = format!("{:?}", root.descendants());
    let walk = format!("{:?}", root.walk());
    assert!(children.starts_with("Children { next: "), "{children}");
    assert_eq!(
        descendants,
        format!("Descendants {{ next: 0, end: {} }}", tree.len())
    );
    assert!(walk.starts_with("Walk { root: 0, next: 0, end: "), "{walk}");
    for s in [children, descendants, walk] {
        assert!(!s.contains("Mapping") && !s.contains("source"), "{s}");
    }
}

#[test]
fn stamps_are_64_bit_and_fail_closed() {
    let a = SyntaxTree::parse("a");
    let b = SyntaxTree::parse("a");
    let id = a.root().id();
    assert!(a.get(id).is_ok());
    assert!(b.get(id).is_err());
    // After the counter is exhausted, trees get a stamp that resolves no
    // handle, not even their own.
    let mut c = SyntaxTree::parse("a");
    c.stamp = super::EXHAUSTED;
    let own = c.root().id();
    assert!(c.get(own).is_err());
    let mut d = SyntaxTree::parse("a");
    d.stamp = super::EXHAUSTED;
    assert!(d.get(own).is_err());
    assert_eq!(std::mem::size_of_val(&own.tree), 8);
}

#[test]
fn error_node_diagnostic_is_the_error_even_after_a_warning() {
    // `%Y` warns (reserved directive) right before the error: both are
    // attached to the error node; its own diagnostic is the error.
    let tree = SyntaxTree::parse("%Y");
    let error = tree
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Error)
        .unwrap();
    let d = error.diagnostic().unwrap();
    assert_eq!(d.severity, oneyaml_core::Severity::Error);
    let by_element: Vec<_> = tree.diagnostics_by_element().collect();
    assert_eq!(by_element.len(), tree.diagnostics().len());
    assert!(
        by_element
            .iter()
            .all(|(n, _)| n.is_some_and(|n| n == error))
    );
    assert_eq!(tree.root().preorder_index(), 0);
    assert!(error.preorder_index() > error.parent().unwrap().preorder_index());
}

/// Windows re-read on this thread so far (see `validate::REREADS`).
fn rereads() -> usize {
    super::validate::REREADS.with(core::cell::Cell::get)
}

/// Removes the white space and comment after the first token whose text
/// is `after` (siblings), returning how many windows the check re-read.
fn remove_comment_after(tree: &mut SyntaxTree, after: &str) -> Result<usize, TreeEditError> {
    let c = tree
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Comment)
        .unwrap();
    let ws = c.prev_sibling().unwrap();
    assert_eq!(ws.kind(), SyntaxKind::Whitespace, "{after:?}");
    let tokens: Vec<Node<'_>> = tree.root().descendants().filter(|n| n.is_token()).collect();
    let i = tokens.iter().position(|t| *t == ws).unwrap();
    assert_eq!(tokens[i - 1].text(), after, "{after:?}");
    let (a, b) = (ws.id(), c.id());
    let start = rereads();
    let _ = tree.remove_range(a, b)?;
    Ok(rereads() - start)
}

/// The trivia shortcut's relaxation — a slice starting with a line break
/// right after content lexes as in place — pinned with fixed cases. A
/// same-line comment (and the white space before it) removed after a
/// closing token (quoted scalar, flow bracket, `:`, `-`, a property,
/// `...`) is decided locally, re-reading no window, in LF, CRLF and CR;
/// after a plain scalar (which could read the next line) it falls back to
/// the window; and a `\r` inserted right before the original `\n` (the
/// two would read as one `\r\n`) is refused.
#[test]
fn comment_removed_after_closing_tokens_is_decided_locally() {
    let cases: &[(&str, &str)] = &[
        ("a: 'x' # c\nb: 1\n", "'x'"),
        ("a: \"x\" # c\nb: 1\n", "\"x\""),
        ("a: [1] # c\nb: 1\n", "]"),
        ("a: {k: v} # c\nb: 1\n", "}"),
        ("a: # c\n  b: 1\n", ":"),
        ("- # c\n  a: 1\n", "-"),
        ("a: &x # c\n  b: 1\n", "&x"),
        ("a: !!map # c\n  b: 1\n", "!!map"),
        ("a: 1\n... # c\n", "..."),
    ];
    for nl in ["\n", "\r\n", "\r"] {
        for (src, after) in cases {
            let src = src.replace('\n', nl);
            let mut tree = SyntaxTree::parse(src.as_str());
            assert!(!tree.has_errors(), "{src:?}");
            let n =
                remove_comment_after(&mut tree, after).unwrap_or_else(|e| panic!("{src:?}: {e:?}"));
            assert_eq!(n, 0, "{src:?}: the shortcut re-read {n} window(s)");
            assert_eq!(tree, SyntaxTree::parse(tree.emit()), "{src:?}");
            assert!(!tree.emit().contains('#'), "{src:?}");
        }
        // After a plain scalar the window decides (a plain scalar could
        // read the next line once the comment is gone).
        let src = "a: x # c\nb: 1\n".replace('\n', nl);
        let mut tree = SyntaxTree::parse(src.as_str());
        let n = remove_comment_after(&mut tree, "x").unwrap();
        assert!(
            n >= 1,
            "{src:?}: a plain scalar must fall back to the window"
        );
        assert_eq!(tree, SyntaxTree::parse(tree.emit()), "{src:?}");
    }
    // A `\r` put in place of the comment right before the original `\n`
    // would read as one `\r\n` line break: refused, the tree unchanged.
    let src = "a: 'x' # c\nb: 1\n";
    let mut tree = SyntaxTree::parse(src);
    let c = find(&tree, SyntaxKind::Comment);
    let (a, b) = (c.prev_sibling().unwrap().id(), c.id());
    let cr = Fragment::parse("\r").unwrap();
    assert_eq!(cr.root_kinds().collect::<Vec<_>>(), [SyntaxKind::LineBreak]);
    assert_eq!(
        tree.replace_range(a, b, cr),
        Err(TreeEditError::NotWellFormed)
    );
    assert_eq!(tree.emit(), src);
    // The same `\r` at the end of the line of a CR document is two
    // breaks, and accepted.
    let src = "a: 'x' # c\rb: 1\r";
    let mut tree = SyntaxTree::parse(src);
    let c = find(&tree, SyntaxKind::Comment);
    let (a, b) = (c.prev_sibling().unwrap().id(), c.id());
    let _ = tree
        .replace_range(a, b, Fragment::parse("\r").unwrap())
        .unwrap();
    assert_eq!(tree.emit(), "a: 'x'\r\rb: 1\r");
    assert_eq!(tree, SyntaxTree::parse(tree.emit()));
}

/// Removes the whole line of the first comment whose text is `text`: the
/// white space before it (its indentation), the comment, and its line
/// break; returns how many windows the check re-read.
fn remove_comment_line(tree: &mut SyntaxTree, text: &str) -> Result<usize, TreeEditError> {
    let c = tree
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Comment && n.text() == text)
        .unwrap();
    let first = c
        .prev_sibling()
        .filter(|w| w.kind() == SyntaxKind::Whitespace)
        .unwrap_or(c);
    let lb = c.next_sibling().unwrap();
    assert_eq!(lb.kind(), SyntaxKind::LineBreak, "{text:?}");
    let (a, b) = (first.id(), lb.id());
    let start = rereads();
    let _ = tree.remove_range(a, b)?;
    Ok(rereads() - start)
}

/// Pins the two trivia-shortcut relaxations (trivia edits
/// at document or stream level stay local):
/// `trivia_only` decides edits at the start of the text or of its first
/// document, and whole comment lines removed right before a node that
/// starts its line, without re-reading a window (`next_line_start` is true
/// at the start of the text), in LF, CRLF and CR. A whole comment line
/// removed after a plain scalar still falls back to the window (the
/// scalar could read the next line), and edits whose text would read
/// differently are refused, the tree unchanged.
#[test]
fn trivia_edits_at_the_start_and_whole_lines_are_decided_locally() {
    for nl in ["\n", "\r\n", "\r"] {
        // Decided locally: whole comment lines.
        let local: &[(&str, &str, &str)] = &[
            // At the start of the text (above the root).
            ("# c\na: 1\n", "# c", "a: 1\n"),
            ("# c\n# d\na: 1\n", "# c", "# d\na: 1\n"),
            ("# c\n- a\n", "# c", "- a\n"),
            // Before an entry that starts its line (after a closing token:
            // a plain scalar before is the fallback below).
            ("a: '1'\n# c\nb: 2\n", "# c", "a: '1'\nb: 2\n"),
            (
                "k:\n  a: [1]\n  # c\n  b: 2\n",
                "# c",
                "k:\n  a: [1]\n  b: 2\n",
            ),
            ("a: 1 # t\n# c\nb: 2\n", "# c", "a: 1 # t\nb: 2\n"),
            ("- 'a'\n  # c\n- b\n", "# c", "- 'a'\n- b\n"),
        ];
        for (src, c, want) in local {
            let (src, want) = (src.replace('\n', nl), want.replace('\n', nl));
            let mut tree = SyntaxTree::parse(src.as_str());
            assert!(!tree.has_errors(), "{src:?}");
            let n = remove_comment_line(&mut tree, c).unwrap_or_else(|e| panic!("{src:?}: {e:?}"));
            assert_eq!(n, 0, "{src:?}: the shortcut re-read {n} window(s)");
            assert_eq!(tree.emit(), want, "{src:?}");
            assert_eq!(tree, SyntaxTree::parse(tree.emit()), "{src:?}");
        }
        // A comment line inserted at the start of the text.
        let src = "a: 1\n".replace('\n', nl);
        let mut tree = SyntaxTree::parse(src.as_str());
        let first = tree.root().first_child().unwrap().id();
        let start = rereads();
        let _ = tree
            .insert_before(first, Fragment::parse(&format!("# n{nl}")).unwrap())
            .unwrap();
        assert_eq!(rereads() - start, 0, "{src:?}: an inserted first line");
        assert_eq!(tree.emit(), format!("# n{nl}a: 1{nl}"));
        assert_eq!(tree, SyntaxTree::parse(tree.emit()));
        // Falls back: a whole comment line after a plain scalar.
        let src = "a: x\n  # c\nb: 1\n".replace('\n', nl);
        let mut tree = SyntaxTree::parse(src.as_str());
        let n = remove_comment_line(&mut tree, "# c").unwrap();
        assert!(
            n >= 1,
            "{src:?}: a plain scalar must fall back to the window"
        );
        assert_eq!(tree.emit(), format!("a: x{nl}b: 1{nl}"));
        assert_eq!(tree, SyntaxTree::parse(tree.emit()));
        // Refused: a comment without its line break at the start of the
        // text would take in the root's first line.
        let src = "a: 1\n".replace('\n', nl);
        let mut tree = SyntaxTree::parse(src.as_str());
        let first = tree.root().first_child().unwrap().id();
        assert_eq!(
            tree.insert_before(first, Fragment::parse("# n").unwrap())
                .map(|_| ()),
            Err(TreeEditError::NotWellFormed),
            "{src:?}"
        );
        assert_eq!(tree.emit(), src);
        // Refused: the line break after a comment removed, so that the
        // comment would take in the next line (at the start of the text,
        // and after an entry): the removal does not start a line.
        for src in ["# c\na: 1\n", "a: '1' # c\nb: 2\n", "a: [1]\n# c\nb: 2\n"] {
            let src = src.replace('\n', nl);
            let mut tree = SyntaxTree::parse(src.as_str());
            let lb = find(&tree, SyntaxKind::Comment)
                .next_sibling()
                .unwrap()
                .id();
            assert_eq!(
                tree.remove(lb).map(|_| ()),
                Err(TreeEditError::NotWellFormed),
                "{src:?} -> {:?}",
                tree.emit()
            );
            assert_eq!(tree.emit(), src);
        }
    }
}

/// Stream-level trivia after a document ending with
/// a block collection is decided by that collection's tail window, never
/// the whole document: a removal before the end of the text (the window
/// then re-reads the element after the gap), and an insertion of whole
/// comment lines past the comment the window is cut at. A removal that
/// would join a comment onto the last scalar's line is still refused.
#[test]
fn document_tail_windows_decide_trivia_lines_after_it() {
    let big: String = (0..300).map(|i| format!("  k{i}: {i}\n")).collect();
    for nl in ["\n", "\r\n", "\r"] {
        // A removal of `  # f⏎` lines (whole lines, not the last line
        // break) after the document.
        let src = format!("a:\n{big}  # f\n  # g\n# h\n").replace('\n', nl);
        let mut tree = SyntaxTree::parse(src.as_str());
        let f = tree
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::Comment && n.text() == "# f")
            .unwrap();
        let (a, b) = (
            f.prev_sibling().unwrap().id(),
            f.next_sibling().unwrap().id(),
        );
        let start = rereads();
        let _ = tree.remove_range(a, b).unwrap();
        assert!(rereads() - start <= 1, "{nl:?}");
        assert_eq!(tree, SyntaxTree::parse(tree.emit()), "{nl:?}");
        assert!(
            tree.emit().ends_with(&format!("  # g{nl}# h{nl}")),
            "{nl:?}"
        );
        // An insertion of two comment lines, the first cut at: before the
        // final line break, after the document.
        let src = format!("a:\n{big}").replace('\n', nl);
        let mut tree = SyntaxTree::parse(src.as_str());
        let lines = SyntaxTree::parse(format!("x: 1{nl}    # f{nl}  # g{nl}").as_str());
        let kids: Vec<Node<'_>> = lines.root().children().collect();
        let first = kids
            .iter()
            .position(|k| k.kind() == SyntaxKind::Whitespace)
            .unwrap();
        let frag = lines
            .extract(kids[first].id(), kids[kids.len() - 1].id())
            .unwrap();
        let last = tree.root().last_child().unwrap().id();
        let start = rereads();
        tree.insert_after(last, frag).unwrap();
        assert!(rereads() - start <= 1, "{nl:?}");
        assert_eq!(tree, SyntaxTree::parse(tree.emit()), "{nl:?}");
    }
    // White space inserted right after
    // the cut comment, on its line, is that comment's text on re-read
    // (§6.6): never accepted as a token of its own, after any block root
    // with a cut comment, before `---`, after `|+`, in LF, CRLF and CR.
    for nl in ["\n", "\r\n", "\r"] {
        for body in [
            "a: 1\n# c\n",
            "a: 1\n# c\n--- b\n",
            "a: |+\n  x\n\n# c\n",
            "- a\n# c\n",
        ] {
            for ins in ["  \n", "\t\n", "  \n# z\n"] {
                let (body, ins) = (body.replace('\n', nl), ins.replace('\n', nl));
                let tree = SyntaxTree::parse(body.as_str());
                let c = tree
                    .root()
                    .descendants()
                    .find(|n| n.kind() == SyntaxKind::Comment)
                    .unwrap()
                    .id();
                for before_break in [false, true] {
                    let mut t = tree.fork();
                    let frag = Fragment::parse(ins.as_str()).unwrap();
                    let r = if before_break {
                        let b = t.get(c).unwrap().next_sibling().unwrap().id();
                        t.insert_before(b, frag)
                    } else {
                        t.insert_after(c, frag)
                    };
                    if r.is_ok() {
                        assert_eq!(t, SyntaxTree::parse(t.emit()), "{body:?} + {ins:?}");
                    } else {
                        assert_eq!(t.emit(), body, "{body:?} + {ins:?}");
                    }
                }
            }
        }
    }
    // Removing the line break between the last scalar and a comment line
    // would read `1# c` as one plain scalar: refused, nothing changed.
    let src = "a: 1\n# c\n";
    let mut tree = SyntaxTree::parse(src);
    let c = tree
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Comment)
        .unwrap();
    let lb = c.prev_sibling().unwrap();
    assert_eq!(lb.kind(), SyntaxKind::LineBreak);
    let id = lb.id();
    assert!(tree.remove_range(id, id).is_err());
    assert_eq!(tree.emit(), src);
}

/// Directives held in the arena build exactly the tree
/// buffering them built, at every arena cap (the `Document` adopting them
/// is inserted before them) and under every diagnostics limit.
#[test]
fn held_directives_build_the_buffered_tree() {
    let sources = [
        "%YAML 1.2\n--- a\n",
        "%YAML 1.2\n...\n%YAML 1.2\n...\n",
        "%A\n%B c\n# x\n\n%TAG !e! tag:e,1:\n--- !e!x [1, 2]\n...\n%YAML 1.1\n---\nb\n",
        "%YAML 1.2\n%YAML 1.2\n--- a\n",
        "a\n%YAML 1.2\n---\nb\n",
        "%YAML 2.0\n%FOO\n",
        "%YAML 1.2\n- a\n",
        "--- a\n...\n%A\n%A\n%A\n--- &x b\n...\n%A\n",
        "%TAG ! !foo\n%TAG ! !bar\n--- !x a\n",
    ];
    for src in sources {
        let events = parse(src);
        for max in [0usize, 1, 2, usize::MAX] {
            let reference = {
                let mut b = Builder::new(src.len(), Origin::Parser)
                    .max_diagnostics(max)
                    .buffered();
                for e in events.iter() {
                    b.push(e.clone());
                }
                b.finish()
            };
            let total = reference.kinds.len();
            for cap in (1..total + 3).chain([usize::MAX]) {
                let build = |held: bool| {
                    let mut b =
                        Builder::with_cap(src.len(), cap, Origin::Parser).max_diagnostics(max);
                    if !held {
                        b = b.buffered();
                    }
                    for e in events.iter() {
                        b.push(e.clone());
                    }
                    b.finish()
                };
                let held = build(true);
                let buffered = build(false);
                assert_eq!(held, buffered, "{src:?} cap {cap} max {max}");
                let tree = SyntaxTree::from_parts(src.to_owned(), held, Settings::new());
                assert_eq!(tree.emit(), src, "{src:?} cap {cap}");
            }
        }
        checked(src);
    }
}

/// Bytes of edited text re-read on this thread so far (see
/// `validate::REREAD_BYTES`).
fn reread_bytes() -> usize {
    super::validate::REREAD_BYTES.with(core::cell::Cell::get)
}

/// The first node of `kind` whose text is `text`, in preorder.
fn node_with(tree: &SyntaxTree, kind: SyntaxKind, text: &str) -> NodeId {
    tree.root()
        .descendants()
        .find(|n| n.kind() == kind && n.text() == text)
        .unwrap()
        .id()
}

/// The siblings from the line break after the first item of the
/// synthetic `- x⏎{item}` sequence to its end: an item appended as the
/// `Document` layer appends one.
fn appended_item(item: &str) -> Fragment {
    let t = SyntaxTree::parse(format!("- x\n{item}"));
    let seq = t
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::BlockSequence)
        .unwrap();
    let lb = seq
        .children()
        .find(|n| n.kind() == SyntaxKind::LineBreak)
        .unwrap();
    t.extract(lb.id(), seq.last_child().unwrap().id()).unwrap()
}

/// Runs `edit` on a fresh tree of `src`: the bytes its check re-read,
/// and the edited text (`None`: refused). An accepted edit reads
/// back as its own text: the edited tree equals the tree parsed from its
/// text under its settings, structurally (`compare_trees`: every token,
/// white space included, the diagnostics, headers and tags). An
/// earlier check here dropped white space and missed tabs written
/// into indentation the validation never re-read.
fn reread_by(
    src: &str,
    edit: impl FnOnce(&mut SyntaxTree) -> Result<(), TreeEditError>,
) -> (usize, Option<String>) {
    let mut tree = SyntaxTree::parse(src);
    let start = reread_bytes();
    let r = edit(&mut tree);
    let bytes = reread_bytes() - start;
    if r.is_err() {
        return (bytes, None);
    }
    let text = tree.emit();
    assert_reads_back(&tree);
    (bytes, Some(text))
}

/// An edit right before an entry written on one long
/// line (`big: [0, 1, …]`) peeks at that line's start only (the parser
/// decides a node's head within `KEY_WINDOW` bytes): before, the whole
/// line was re-read (about 1 s for 4M items).
#[test]
fn a_peek_reads_a_bounded_prefix_of_a_long_line() {
    let src = |items: usize| {
        let mut s = String::from("a: 1\nbig: [");
        for i in 0..items {
            s.push_str(&format!("{}, ", i % 10));
        }
        s.push_str("0]\nz: 2\n");
        s
    };
    let large = src(400_000);
    assert!(large.len() > 1_000_000);
    let (bytes, text) = reread_by(&large, |t| {
        let one = node_with(t, SyntaxKind::Scalar, "1");
        t.replace(one, Fragment::parse("3").unwrap()).map(|_| ())
    });
    assert_eq!(text.unwrap(), large.replacen("a: 1", "a: 3", 1));
    assert!(bytes < 20_000, "re-read {bytes} bytes of a 1 MB line");
    // A short line is still read whole.
    let small = src(10);
    let (bytes, text) = reread_by(&small, |t| {
        let one = node_with(t, SyntaxKind::Scalar, "1");
        t.replace(one, Fragment::parse("3").unwrap()).map(|_| ())
    });
    assert_eq!(text.unwrap(), small.replacen("a: 1", "a: 3", 1));
    assert!(bytes >= small.find("\nz").unwrap() - "a: 1\n".len());
}

/// Removing a mapping's first entry makes the next one
/// set the mapping's column; when that column is unchanged, its value is
/// peeked, not re-read whole (`move_entry` of a first entry past a large
/// second one re-read that whole entry).
#[test]
fn removing_a_first_entry_peeks_at_the_next_value() {
    for (pre, ind) in [("", ""), ("top:\n", "  ")] {
        let mut src = format!("{pre}{ind}k0: 0\n{ind}big:\n");
        for i in 0..50_000 {
            src.push_str(&format!("{ind}  b{i}: {i}\n"));
        }
        src.push_str(&format!("{ind}z: 1\n"));
        let (bytes, text) = reread_by(&src, |t| {
            let k0 = node_with(t, SyntaxKind::Scalar, "k0");
            let big = node_with(t, SyntaxKind::Scalar, "big");
            let last = t.get(big).unwrap().prev_sibling().unwrap().id();
            t.remove_range(k0, last).map(|_| ())
        });
        assert_eq!(text.unwrap(), src.replacen(&format!("k0: 0\n{ind}"), "", 1));
        assert!(bytes < 1_000, "{pre:?}: re-read {bytes} bytes");
    }
}

/// An item appended after a last item that ends with a
/// closed token (a flow collection, a quoted scalar) or a plain scalar,
/// and its line, is read from its own line: the last item, which ends
/// its entry whatever follows its line, is not re-read.
#[test]
fn appending_after_a_long_closed_item_reads_the_new_line() {
    let long = |open: &str, close: &str| {
        let mut s = String::from("- 0\n- ");
        s.push_str(open);
        for i in 0..100_000 {
            s.push_str(&format!("w{i} "));
        }
        s.push_str("end");
        s.push_str(close);
        s.push('\n');
        s
    };
    for (open, close) in [("[", "]"), ("'", "'"), ("", "")] {
        let src = long(open, close);
        let (bytes, text) = reread_by(&src, |t| {
            let seq = t
                .root()
                .descendants()
                .find(|n| n.kind() == SyntaxKind::BlockSequence)
                .unwrap();
            let last = seq.last_child().unwrap().id();
            t.insert_after(last, appended_item("- 1")).map(|_| ())
        });
        assert_eq!(text.unwrap(), format!("{src}- 1\n"));
        assert!(bytes < 100, "{open:?}: re-read {bytes} bytes");
    }
    // A `#` right after the closed token is no comment in place: refused
    // (as before), whatever the trivia check says on its own.
    let src = "- 0\n- [a]\n";
    let (_, text) = reread_by(src, |t| {
        let close = node_with(t, SyntaxKind::FlowSequence, "[a]");
        let frag = {
            let f = SyntaxTree::parse("- x #c\n- 1");
            let seq = f
                .root()
                .descendants()
                .find(|n| n.kind() == SyntaxKind::BlockSequence)
                .unwrap();
            let c = f
                .root()
                .descendants()
                .find(|n| n.kind() == SyntaxKind::Comment)
                .unwrap();
            f.extract(c.id(), seq.last_child().unwrap().id())
        };
        match frag {
            Ok(frag) => t.insert_after(close, frag).map(|_| ()),
            Err(e) => Err(e),
        }
    });
    assert_eq!(text, None, "`]#c` accepted");
}

/// The same-line comment after a plain scalar that ends
/// its document is removed as trivia (no line after it can continue the
/// scalar when no content follows in its document); before, appending
/// after a last item `- text # c` re-read that item.
#[test]
fn a_comment_after_a_document_final_plain_scalar() {
    for tail in ["", "# more\n\n", "---\nnext: 1\n"] {
        let mut src = String::from("- 0\n- ");
        for i in 0..100_000 {
            src.push_str(&format!("w{i} "));
        }
        src.push_str("end # c\n");
        src.push_str(tail);
        let mut tree = SyntaxTree::parse(src.as_str());
        let plain = tree
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::PlainScalar && n.text().ends_with("end"))
            .unwrap();
        let plain = plain.text().into_owned();
        let (a, b) = (reread_bytes(), rereads());
        remove_comment_after(&mut tree, &plain).unwrap();
        assert_eq!(rereads(), b, "{tail:?}: re-read a window");
        assert_eq!(reread_bytes(), a);
        assert_eq!(tree.emit(), src.replacen(" # c", "", 1));
    }
    // Content after it in its document could continue it (or not): read
    // as before (a window), and refused or accepted as the text reads.
    let src = "k: a # c\n  # d\nj: 1\n";
    // The exact outcome (accepted, through one window: the
    // comment line at column 2 ends the scalar either way).
    let mut tree = SyntaxTree::parse(src);
    assert_eq!(remove_comment_after(&mut tree, "a"), Ok(1));
    assert_eq!(tree.emit(), "k: a\n  # d\nj: 1\n");
    assert_reads_back(&tree);
}

/// An edited tree always agrees with its own text:
/// `tree` equals the tree parsed from its text under its settings,
/// structurally (every token, white space included; diagnostics).
fn assert_reads_back(tree: &SyntaxTree) {
    let text = tree.emit();
    let again = SyntaxTree::parse_with(text.as_str(), tree.settings());
    if let Err(e) = compare_trees(&again, tree) {
        panic!("{text:?} reads back differently: {e}");
    }
}

/// Edits of the indentation before the line after a value
/// (and of the line break before it), after plain, quoted and flow
/// values, in block sequences and mappings, at column 0 and indented.
/// `(source, edit, text, outcome)`: the edit is on the token after the
/// line break ending the first entry's line (the second line break for
/// sources starting with `k:`): `rep` replaces it (white space) with the
/// text, `del` removes it, `before`/`after` insert the text next to it;
/// `lbafter` inserts the text after the line break (no indentation
/// there). The outcome is the edited text, or `None` when refused: in
/// every case the outcome of the line-by-line check. The token
/// shortcut once accepted the same-length tab mixes (` - a⏎\t- b`), whose text
/// reads back with errors: the shortcut past the value wrote the line's
/// indentation as spaces and never re-read it.
const INDENTATION_EDITS: &[(&str, &str, &str, Option<&str>)] = &[
    (" - a\n - b\n - c\n", "rep", "\t", None),
    (" - a\n - b\n - c\n", "rep", " ", Some(" - a\n - b\n - c\n")),
    (" - a\n - b\n - c\n", "rep", "  ", None),
    (" - a\n - b\n - c\n", "del", "", None),
    (" - a\n - b\n - c\n", "before", " ", None),
    (" - a\n - b\n - c\n", "after", " ", None),
    (" - a\n - b\n - c\n", "before", "\t", None),
    (" - a\n - b\n - c\n", "after", "\t", None),
    (" - 'a'\n - b\n", "rep", "\t", None),
    (" - 'a'\n - b\n", "rep", " ", Some(" - 'a'\n - b\n")),
    (" - 'a'\n - b\n", "rep", "  ", None),
    (" - 'a'\n - b\n", "del", "", None),
    (" - 'a'\n - b\n", "before", " ", None),
    (" - 'a'\n - b\n", "after", " ", None),
    (" - 'a'\n - b\n", "before", "\t", None),
    (" - 'a'\n - b\n", "after", "\t", None),
    (" - \"a\"\n - b\n", "rep", "\t", None),
    (" - \"a\"\n - b\n", "rep", " ", Some(" - \"a\"\n - b\n")),
    (" - \"a\"\n - b\n", "rep", "  ", None),
    (" - \"a\"\n - b\n", "del", "", None),
    (" - \"a\"\n - b\n", "before", " ", None),
    (" - \"a\"\n - b\n", "after", " ", None),
    (" - \"a\"\n - b\n", "before", "\t", None),
    (" - \"a\"\n - b\n", "after", "\t", None),
    (" - [a]\n - b\n", "rep", "\t", None),
    (" - [a]\n - b\n", "rep", " ", Some(" - [a]\n - b\n")),
    (" - [a]\n - b\n", "rep", "  ", None),
    (" - [a]\n - b\n", "del", "", None),
    (" - [a]\n - b\n", "before", " ", None),
    (" - [a]\n - b\n", "after", " ", None),
    (" - [a]\n - b\n", "before", "\t", None),
    (" - [a]\n - b\n", "after", "\t", None),
    (" - a # c\n - b\n", "rep", "\t", None),
    (" - a # c\n - b\n", "rep", " ", Some(" - a # c\n - b\n")),
    (" - a # c\n - b\n", "rep", "  ", None),
    (" - a # c\n - b\n", "del", "", None),
    (" - a # c\n - b\n", "before", " ", None),
    (" - a # c\n - b\n", "after", " ", None),
    (" - a # c\n - b\n", "before", "\t", None),
    (" - a # c\n - b\n", "after", "\t", None),
    (" a: 1\n b: 2", "rep", "\t", None),
    (" a: 1\n b: 2", "rep", " ", Some(" a: 1\n b: 2")),
    (" a: 1\n b: 2", "rep", "  ", None),
    (" a: 1\n b: 2", "del", "", None),
    (" a: 1\n b: 2", "before", " ", None),
    (" a: 1\n b: 2", "after", " ", None),
    (" a: 1\n b: 2", "before", "\t", None),
    (" a: 1\n b: 2", "after", "\t", None),
    (" a: 'x'\n b: 2\n", "rep", "\t", None),
    (" a: 'x'\n b: 2\n", "rep", " ", Some(" a: 'x'\n b: 2\n")),
    (" a: 'x'\n b: 2\n", "rep", "  ", None),
    (" a: 'x'\n b: 2\n", "del", "", None),
    (" a: 'x'\n b: 2\n", "before", " ", None),
    (" a: 'x'\n b: 2\n", "after", " ", None),
    (" a: 'x'\n b: 2\n", "before", "\t", None),
    (" a: 'x'\n b: 2\n", "after", "\t", None),
    (" a: [x]\n b: 2\n", "rep", "\t", None),
    (" a: [x]\n b: 2\n", "rep", " ", Some(" a: [x]\n b: 2\n")),
    (" a: [x]\n b: 2\n", "rep", "  ", None),
    (" a: [x]\n b: 2\n", "del", "", None),
    (" a: [x]\n b: 2\n", "before", " ", None),
    (" a: [x]\n b: 2\n", "after", " ", None),
    (" a: [x]\n b: 2\n", "before", "\t", None),
    (" a: [x]\n b: 2\n", "after", "\t", None),
    (" a: {x: 1} # c\n b: 2\n", "rep", "\t", None),
    (
        " a: {x: 1} # c\n b: 2\n",
        "rep",
        " ",
        Some(" a: {x: 1} # c\n b: 2\n"),
    ),
    (" a: {x: 1} # c\n b: 2\n", "rep", "  ", None),
    (" a: {x: 1} # c\n b: 2\n", "del", "", None),
    (" a: {x: 1} # c\n b: 2\n", "before", " ", None),
    (" a: {x: 1} # c\n b: 2\n", "after", " ", None),
    (" a: {x: 1} # c\n b: 2\n", "before", "\t", None),
    (" a: {x: 1} # c\n b: 2\n", "after", "\t", None),
    ("k:\n    - a\n    - b", "rep", "\t\t\t\t", None),
    (
        "k:\n    - a\n    - b",
        "rep",
        "    ",
        Some("k:\n    - a\n    - b"),
    ),
    ("k:\n    - a\n    - b", "rep", "     ", None),
    ("k:\n    - a\n    - b", "rep", "   ", None),
    ("k:\n    - a\n    - b", "rep", "   \t", None),
    ("k:\n    - a\n    - b", "rep", "\t   ", None),
    ("k:\n    - a\n    - b", "rep", "  \t ", None),
    ("k:\n    - a\n    - b", "del", "", None),
    ("k:\n    - a\n    - b", "before", " ", None),
    ("k:\n    - a\n    - b", "after", " ", None),
    ("k:\n    - a\n    - b", "before", "\t", None),
    ("k:\n    - a\n    - b", "after", "\t", None),
    ("k:\n    - [a]\n    - b\n", "rep", "\t\t\t\t", None),
    (
        "k:\n    - [a]\n    - b\n",
        "rep",
        "    ",
        Some("k:\n    - [a]\n    - b\n"),
    ),
    ("k:\n    - [a]\n    - b\n", "rep", "     ", None),
    ("k:\n    - [a]\n    - b\n", "rep", "   ", None),
    ("k:\n    - [a]\n    - b\n", "rep", "   \t", None),
    ("k:\n    - [a]\n    - b\n", "rep", "\t   ", None),
    ("k:\n    - [a]\n    - b\n", "rep", "  \t ", None),
    ("k:\n    - [a]\n    - b\n", "del", "", None),
    ("k:\n    - [a]\n    - b\n", "before", " ", None),
    ("k:\n    - [a]\n    - b\n", "after", " ", None),
    ("k:\n    - [a]\n    - b\n", "before", "\t", None),
    ("k:\n    - [a]\n    - b\n", "after", "\t", None),
    ("k:\n    a: 1\n    b: 2\n", "rep", "\t\t\t\t", None),
    (
        "k:\n    a: 1\n    b: 2\n",
        "rep",
        "    ",
        Some("k:\n    a: 1\n    b: 2\n"),
    ),
    ("k:\n    a: 1\n    b: 2\n", "rep", "     ", None),
    ("k:\n    a: 1\n    b: 2\n", "rep", "   ", None),
    ("k:\n    a: 1\n    b: 2\n", "rep", "   \t", None),
    ("k:\n    a: 1\n    b: 2\n", "rep", "\t   ", None),
    ("k:\n    a: 1\n    b: 2\n", "rep", "  \t ", None),
    ("k:\n    a: 1\n    b: 2\n", "del", "", None),
    ("k:\n    a: 1\n    b: 2\n", "before", " ", None),
    ("k:\n    a: 1\n    b: 2\n", "after", " ", None),
    ("k:\n    a: 1\n    b: 2\n", "before", "\t", None),
    ("k:\n    a: 1\n    b: 2\n", "after", "\t", None),
    ("- [a]\n- b\n", "lbafter", " ", None),
    ("- [a]\n- b\n", "lbafter", "\t", None),
    ("- a\n- b\n", "lbafter", " ", None),
    ("- a\n- b\n", "lbafter", "\t", None),
    ("a: 'x'\nb: 2\n", "lbafter", " ", None),
    ("a: 'x'\nb: 2\n", "lbafter", "\t", None),
];

#[test]
fn indentation_after_a_value_is_read_back() {
    for &(src, op, text, want) in INDENTATION_EDITS {
        let mut tree = SyntaxTree::parse(src);
        let tokens: Vec<NodeId> = tree
            .root()
            .descendants()
            .filter(|n| n.is_token())
            .map(|n| n.id())
            .collect();
        let breaks: Vec<usize> = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| tree.get(**t).unwrap().kind() == SyntaxKind::LineBreak)
            .map(|(i, _)| i)
            .collect();
        let lb = breaks[usize::from(src.starts_with("k:"))];
        let (lb, next) = (tokens[lb], tokens[lb + 1]);
        let r = match op {
            "rep" => tree.replace_token(next, text).map(|_| ()),
            "del" => tree.remove(next).map(|_| ()),
            "before" => tree
                .insert_before(next, Fragment::parse(text).unwrap())
                .map(|_| ()),
            "after" => tree
                .insert_after(next, Fragment::parse(text).unwrap())
                .map(|_| ()),
            _ => tree
                .insert_after(lb, Fragment::parse(text).unwrap())
                .map(|_| ()),
        };
        let got = r.is_ok().then(|| tree.emit());
        assert_eq!(got.as_deref(), want, "{src:?} {op} {text:?}");
        match r {
            Ok(()) => assert_reads_back(&tree),
            Err(e) => {
                assert_eq!(e, TreeEditError::NotWellFormed, "{src:?} {op} {text:?}");
                assert_eq!(tree.emit(), src);
            }
        }
    }
    // The known repros, through `reread_by` too.
    for (src, ws, tab) in [
        (" - a\n - b\n - c\n", " ", "\t"),
        (" a: 1\n b: 2", " ", "\t"),
        ("k:\n    - a\n    - b", "    ", "\t\t\t\t"),
        ("k:\n    - a\n    - b", "    ", "  \t "),
    ] {
        let (_, text) = reread_by(src, |t| {
            // The indentation of the last line.
            let w = t
                .root()
                .descendants()
                .filter(|n| {
                    n.kind() == SyntaxKind::Whitespace
                        && n.text() == ws
                        && src[..n.span().start].ends_with('\n')
                })
                .last()
                .unwrap()
                .id();
            t.replace_token(w, tab).map(|_| ())
        });
        assert_eq!(text, None, "{src:?}: {tab:?} accepted");
    }
}

/// Appending after a long last item of an indented
/// sequence (indentation written as spaces) still reads only the new
/// line, as at column 0 (`appending_after_a_long_closed_item_…`).
#[test]
fn appending_after_a_long_indented_item_reads_the_new_line() {
    let item = {
        let t = SyntaxTree::parse("k:\n  - x\n  - 1\n");
        let seq = t
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::BlockSequence)
            .unwrap();
        let lb = seq
            .children()
            .find(|n| n.kind() == SyntaxKind::LineBreak)
            .unwrap();
        t.extract(lb.id(), seq.last_child().unwrap().id()).unwrap()
    };
    for (open, close) in [("[", "]"), ("'", "'"), ("", "")] {
        let mut src = format!("k:\n  - 0\n  - {open}");
        for i in 0..100_000 {
            src.push_str(&format!("w{i} "));
        }
        src.push_str(&format!("end{close}\n"));
        let (bytes, text) = reread_by(&src, |t| {
            let seq = t
                .root()
                .descendants()
                .find(|n| n.kind() == SyntaxKind::BlockSequence)
                .unwrap();
            let last = seq.last_child().unwrap().id();
            t.insert_after(last, item.clone()).map(|_| ())
        });
        assert_eq!(text.unwrap(), format!("{src}  - 1\n"));
        assert!(bytes < 100, "{open:?}: re-read {bytes} bytes");
    }
}

/// A refused edit right before an entry written on one long line is refused as
/// by the line-by-line check; the cut window is refused, then the window with
/// the peek's whole line is re-read too (`reread_uncut`, a safety net), so a
/// refusal there costs the line, as before. The accepted edit beside it reads a
/// bounded prefix (`a_peek_reads_a_bounded_prefix_of_a_long_line`).
#[test]
fn a_refusal_next_to_a_long_line_reads_the_line() {
    let mut src = String::from("a: 1\nbig: [");
    for i in 0..100_000 {
        src.push_str(&format!("{}, ", i % 10));
    }
    src.push_str("0]\nz: 2\n");
    let line = src["a: 1\n".len()..].find('\n').unwrap();
    let mut tree = SyntaxTree::parse(src.as_str());
    let one = node_with(&tree, SyntaxKind::Scalar, "1");
    let (bytes, windows) = (reread_bytes(), rereads());
    // `a: - x` is not well formed (no block sequence on a key's line).
    let r = tree.replace(one, Fragment::parse("- x").unwrap());
    assert_eq!(r.err(), Some(TreeEditError::NotWellFormed));
    assert_eq!(rereads() - windows, 2);
    let read = reread_bytes() - bytes;
    assert!(read > line && read < line + 20_000, "{read} of {line}");
    assert_eq!(tree.emit(), src);
}
