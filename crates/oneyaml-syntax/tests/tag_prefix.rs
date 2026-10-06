//! YAML 1.2.2 §6.8.2.2: a shorthand tag is the URI
//! `%TAG` prefix + suffix, percent-decoded as **one** string. The same URI
//! written with the escape in the prefix, in the suffix, or verbatim has
//! one meaning, in the event stream and in the syntax tree; invalid UTF-8
//! after decoding is `Y0013` wherever the escape sits.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use oneyaml_core::Settings;
use oneyaml_core::code::INVALID_TAG;
use oneyaml_syntax::tree::SyntaxTree;

use common::{events, first_code, first_error};

/// The resolved tag of the first tagged node, from the event stream.
fn event_tag(src: &str) -> String {
    let evs = events(src, &Settings::new());
    let tags: Vec<String> = evs
        .sourced()
        .filter_map(|e| e.tag().map(|t| t.resolved.into_owned()))
        .collect();
    assert!(
        evs.iter().all(|e| !e.is_error()),
        "{src:?}: {:?}",
        first_error(src, &Settings::new())
    );
    tags.into_iter().next().expect("a tag")
}

/// The resolved tag of the first tagged node, from the syntax tree.
fn tree_tag(src: &str) -> String {
    let tree = SyntaxTree::parse(src);
    assert!(!tree.has_errors(), "{src:?}");
    assert_eq!(tree.emit().as_bytes(), src.as_bytes());
    tree.root()
        .descendants()
        .filter(|n| n.kind().is_yaml_node())
        .find_map(|n| n.tag().map(|t| t.resolved.into_owned()))
        .expect("a tag")
}

/// Both readers agree, and give `want`.
fn resolves(src: &str, want: &str) {
    assert_eq!(event_tag(src), want, "events: {src:?}");
    assert_eq!(tree_tag(src), want, "tree: {src:?}");
}

#[test]
fn escaped_prefixes_decode() {
    // The three spellings of `tag:x,1:!a` from the security review.
    resolves("%TAG !e! tag:x,1:%21\n--- !e!a v\n", "tag:x,1:!a");
    resolves("--- !<tag:x,1:%21a> v\n", "tag:x,1:!a");
    resolves("%TAG !e! tag:x,1:\n--- !e!%21a v\n", "tag:x,1:!a");
    // Escapes on both sides; `%25` decodes to `%` once, never twice.
    resolves("%TAG !e! a:%2F%25\n--- !e!b%20c v\n", "a:/%b c");
    resolves("%TAG !e! a:%2541\n--- !e!x v\n", "a:%41x");
    // Lower-case hex, and a collection node, and flow context.
    resolves("%TAG !e! a:%2f\n--- !e!s [x]\n", "a:/s");
    resolves("%TAG !e! a:%21\n--- [!e!x y]\n", "a:!x");
    resolves("%TAG !e! a:%21\n--- {!e!k v: w}\n", "a:!k");
    // A prefix without escapes is unchanged.
    resolves("%TAG !e! tag:e.com,2000:\n--- !e!a v\n", "tag:e.com,2000:a");
}

#[test]
fn multibyte_escapes_split_across_prefix_and_suffix() {
    resolves("%TAG !e! tag:x,1:%C3\n--- !e!%A9 a\n", "tag:x,1:é");
    resolves("--- !<tag:x,1:%C3%A9> a\n", "tag:x,1:é");
    // A four-byte character split 1 + 3 and 3 + 1.
    resolves("%TAG !e! u:%F0\n--- !e!%9F%98%80 a\n", "u:\u{1F600}");
    resolves("%TAG !e! u:%F0%9F%98\n--- !e!%80 a\n", "u:\u{1F600}");
}

#[test]
fn invalid_utf8_prefixes_are_y0013() {
    let s = Settings::new();
    for src in [
        // Invalid in the prefix alone.
        "%TAG !e! %FF\n--- !e!x a\n",
        "%TAG !e! tag:x,1:%FF\n--- !e!x a\n",
        // A lead byte in the prefix with no continuation.
        "%TAG !e! tag:x,1:%C3\n--- !e!x a\n",
        "%TAG !e! tag:x,1:%C3\n--- !e!%41 a\n",
        // Split, but not a valid sequence together.
        "%TAG !e! tag:x,1:%C3\n--- !e!%C3 a\n",
        // The `!` handle overridden with an escape.
        "%TAG ! !%FF\n--- !x a\n",
        // The `!!` handle overridden with an escape.
        "%TAG !! tag:yaml.org,2002:%FF\n--- !!str a\n",
        // In flow context.
        "%TAG !e! %FF\n--- [!e!x a]\n",
        // As before prefix decoding: invalid in the suffix, or verbatim.
        "!a%FF x\n",
        "!<a:%FF> x\n",
    ] {
        assert_eq!(first_code(src, &s), Some(INVALID_TAG), "{src:?}");
        let d = first_error(src, &s).unwrap();
        assert_eq!(
            d.message, "tag is not valid UTF-8 after %-decoding",
            "{src:?}"
        );
        let tree = SyntaxTree::parse(src);
        assert!(tree.has_errors(), "{src:?}");
        assert_eq!(tree.emit().as_bytes(), src.as_bytes());
    }
    // A declared prefix that no tag uses is not an error (§6.8.2.2: the
    // prefix is only a URI prefix; a tag is formed when a node uses it).
    assert_eq!(first_code("%TAG !e! %FF\n--- a\n", &s), None);
}

#[test]
fn default_handle_overrides() {
    // `!` overridden: its escapes are decoded like any prefix.
    resolves("%TAG ! !%21\n--- !x a\n", "!!x");
    resolves("%TAG ! tag:x,1:%C3\n--- !%A9 a\n", "tag:x,1:é");
    // `!` redeclared as itself keeps its own text.
    resolves("%TAG ! !\n--- !x a\n", "!x");
    // `!!` overridden with an escaped Core prefix resolves to the Core tag
    // URI, the same as the default `!!` and the verbatim form.
    resolves(
        "%TAG !! tag:yaml.org%2C2002:\n--- !!int 1\n",
        "tag:yaml.org,2002:int",
    );
    resolves("--- !!int 1\n", "tag:yaml.org,2002:int");
    resolves(
        "--- !<tag:yaml.org%2C2002:int> 1\n",
        "tag:yaml.org,2002:int",
    );
}

/// The directive event keeps its prefix as written: decoding belongs to
/// the tags formed from it, and the event reproduces its bytes.
#[test]
fn directive_prefix_is_kept_as_written() {
    use oneyaml_syntax::event::DirectiveKind;
    let src = "%TAG !e! tag:x,1:%21\n--- !e!a v\n";
    let evs = events(src, &Settings::new());
    let prefix = evs
        .sourced()
        .find_map(|e| match e.directive() {
            Some(DirectiveKind::Tag { prefix, .. }) => Some(prefix),
            _ => None,
        })
        .unwrap();
    assert_eq!(prefix, "tag:x,1:%21");
}

// ============================================================== prefix escapes

/// The `%TAG` prefix (at its directive) and each
/// resolved tag are bounded by `max_scalar_len`, with its code (`Y2001`).
#[test]
fn prefixes_and_resolved_tags_are_bounded_by_max_scalar_len() {
    use oneyaml_core::code::SCALAR_TOO_LONG;
    let lim = |n: usize| Settings::new().limits(oneyaml_core::Limits::new().max_scalar_len(n));
    let s = lim(8);
    // The prefix decodes to 9 bytes: refused at the directive.
    let src = "%TAG !e! a:1234567\n--- !e!x v\n";
    let d = first_error(src, &s).unwrap();
    assert_eq!(d.code, SCALAR_TOO_LONG);
    assert!(d.primary.span.start < src.find("---").unwrap(), "{d:?}");
    // Escapes count decoded: `a:%41%41%41%41%41%41` is 8 bytes.
    assert_eq!(
        first_code("%TAG !e! a:%41%41%41%41%41%41\n--- v\n", &s),
        None
    );
    // Prefix 7 bytes, suffix 1 (8): accepted; suffix 2 (9): refused.
    let ok = "%TAG !e! a:12345\n--- !e!x v\n";
    assert_eq!(first_code(ok, &s), None);
    let long = "%TAG !e! a:12345\n--- !e!xy v\n";
    assert_eq!(first_code(long, &s), Some(SCALAR_TOO_LONG));
    // The default handles and verbatim tags are bounded too.
    assert_eq!(first_code("!!abcdef v\n", &lim(20)), Some(SCALAR_TOO_LONG));
    assert_eq!(first_code("!<a:123456789> v\n", &s), Some(SCALAR_TOO_LONG));
    assert_eq!(
        first_code("[!<a:123456789> v]\n", &s),
        Some(SCALAR_TOO_LONG)
    );
    // Under the defaults nothing changes.
    assert_eq!(first_code(long, &Settings::new()), None);
    let tree = SyntaxTree::parse_with(long, &s);
    assert!(tree.has_errors());
    assert_eq!(tree.emit().as_bytes(), long.as_bytes());
}

/// `TagSource`: reading a tag as written never reads the
/// prefix; its key tells equal resolutions apart; its parts (the
/// the directive's or default prefix, then the decoded suffix) join to
/// what `resolve` gives.
#[test]
fn tag_sources_resolve_like_tags() {
    let src =
        "%TAG !e! tag:x,1:%C3\n---\n- !e!%A9 a\n- !e!%A9 b\n- !e!%80 c\n- !!str d\n- !<u:v> e\n";
    let evs = events(src, &Settings::new());
    let sources: Vec<_> = evs.sourced().filter_map(|e| e.tag_source()).collect();
    assert_eq!(sources.len(), 5);
    for (t, e) in sources
        .iter()
        .zip(evs.sourced().filter(|e| e.tag().is_some()))
    {
        assert_eq!(t.resolve(), e.tag());
        assert_eq!(t.span, e.tag().unwrap().span);
    }
    assert_eq!(sources[0].key(), sources[1].key());
    assert_ne!(sources[0].key(), sources[2].key());
    assert_eq!(sources[0].prefix_key(), sources[2].prefix_key());
    assert!(sources[3].prefix_key().is_none() && sources[4].prefix_key().is_none());
    let prefix = sources[0].directive_prefix().unwrap();
    assert_eq!(&*prefix, b"tag:x,1:\xC3");
    assert_eq!(&*sources[1].decoded_suffix(), b"\xA9");
    assert_eq!(sources[3].default_prefix(), Some("tag:yaml.org,2002:"));
    assert_eq!(sources[0].default_prefix(), None);
    assert_eq!(sources[4].default_prefix(), None);
    for t in &sources {
        let mut joined: Vec<u8> = match (t.directive_prefix(), t.default_prefix()) {
            (Some(p), _) => p.into_owned(),
            (None, Some(d)) => d.as_bytes().to_vec(),
            (None, None) => Vec::new(),
        };
        joined.extend_from_slice(&t.decoded_suffix());
        assert_eq!(
            String::from_utf8(joined).unwrap(),
            t.resolve().unwrap().resolved
        );
    }
    // The tree reads the same sources.
    let tree = SyntaxTree::parse(src);
    let nodes: Vec<_> = tree
        .root()
        .descendants()
        .filter_map(|n| n.tag_source())
        .collect();
    assert_eq!(nodes.len(), 5);
    for (n, t) in nodes.iter().zip(&sources) {
        assert_eq!(n.resolve(), t.resolve());
        assert_eq!(n.span, t.span);
    }
}

/// Tags cost only their own text. With a long `%TAG`
/// prefix, 10× the tags cost about 10× the (small) tag text, never
/// 10× the prefix per tag: the input grows 1.05×, so O(tags × prefix)
/// would make the large run about 10× slower; the bound is 3×.
#[test]
fn tag_validation_does_not_rescan_the_prefix() {
    use std::time::{Duration, Instant};
    fn input(tags: usize) -> String {
        let mut s = format!("%TAG !e! tag:x,1:{}%C3\n---\n", "a".repeat(512 * 1024));
        for _ in 0..tags {
            s.push_str("- !e!%A9 v\n");
        }
        s
    }
    let time = |src: &str| {
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let tree = SyntaxTree::parse(src);
            assert!(!tree.has_errors());
            best = best.min(t.elapsed());
        }
        best
    };
    let (small, large) = (input(200), input(2000));
    let (a, b) = (time(&small), time(&large));
    let ratio = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    println!("200 tags {a:?}, 2000 tags {b:?}: {ratio:.2}x");
    assert!(ratio < 3.0, "{ratio:.2}x");
}
