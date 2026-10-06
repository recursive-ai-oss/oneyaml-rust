#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use super::*;
use oneyaml_core::{Code, Label};

#[test]
fn accessors() {
    let e = Event::new(EventKind::Trivia(TriviaKind::Comment), Span::new(0, 3));
    assert!(e.is_trivia() && !e.is_error() && !e.is_warning());
    assert_eq!(e.source("# c\n"), Some("# c"));
    assert_eq!(e.source("#"), None);
    assert!(e.diagnostic().is_none());
    assert_eq!(e.span(), Span::new(0, 3));
    assert!(e.properties().is_none() && e.node_info().is_none());
    assert!(e.block_header().is_none());
    assert!(e.value("# c").is_none() && e.alias_name("# c").is_none());
    assert!(e.directive("# c").is_none() && e.anchor("# c").is_none());
    assert!(matches!(
        e.into_kind(),
        EventKind::Trivia(TriviaKind::Comment)
    ));

    let d = Diagnostic::warning(
        Code::new(1003).unwrap(),
        "w",
        Label::new(Span::empty(0), ""),
    );
    let w = Event::new(EventKind::Warning(Box::new(d.clone())), Span::empty(0));
    assert!(w.is_warning() && !w.is_error() && !w.is_trivia());
    assert_eq!(w.diagnostic(), Some(&d));
    let x = Event::new(EventKind::Error(Box::new(d.clone())), Span::empty(0));
    assert!(x.is_error());
    assert_eq!(x.diagnostic(), Some(&d));
}

#[test]
fn constructors() {
    assert!(Properties::none().is_empty());
    assert_eq!(Properties::default(), Properties::none());
    assert!(!Properties::new(Some(0), None, None).is_empty());
    assert!(matches!(
        EventKind::document_start(true),
        EventKind::DocumentStart { explicit: true, .. }
    ));
    assert!(matches!(
        EventKind::document_end(false),
        EventKind::DocumentEnd {
            explicit: false,
            ..
        }
    ));
    assert!(matches!(
        EventKind::mapping_start(CollectionStyle::FlowPair),
        EventKind::MappingStart {
            style: CollectionStyle::FlowPair,
            ..
        }
    ));
    assert!(matches!(
        EventKind::sequence_start(CollectionStyle::Flow),
        EventKind::SequenceStart {
            style: CollectionStyle::Flow,
            ..
        }
    ));
    assert!(matches!(
        EventKind::plain(),
        EventKind::Scalar {
            style: ScalarStyle::Plain,
            ..
        }
    ));
    let h = BlockHeader::new(Chomping::Keep, Some(2), 4, Span::new(0, 3));
    assert_eq!(h.indent_indicator, Some(2));
    let t = Tag::new(
        TagForm::NonSpecific,
        "!",
        "",
        Cow::Borrowed("!"),
        Span::new(0, 1),
    );
    assert_eq!(t.resolved, "!");
    assert_eq!(Anchor::new("a", Span::new(0, 2)).name, "a");
    let r = DirectiveKind::reserved("FOO", "a b");
    assert_eq!(r.directive(), Directive::Reserved);
    assert_eq!(DirectiveKind::yaml("1.2").directive(), Directive::Yaml);
    assert_eq!(DirectiveKind::tag("!e!", "p").directive(), Directive::Tag);
    let chars: String = [
        Indicator::SequenceEntry,
        Indicator::MappingKey,
        Indicator::MappingValue,
        Indicator::FlowEntry,
    ]
    .iter()
    .map(|i| i.as_str())
    .collect();
    assert_eq!(chars, "-?:,");
}

#[test]
fn event_is_compact() {
    // Values and names are read from the source on demand, and node
    // positions are packed relative to the node, so an event is 48 bytes.
    if cfg!(target_pointer_width = "64") {
        assert_eq!(core::mem::size_of::<NodeInfo>(), 24);
        assert_eq!(core::mem::size_of::<EventKind>(), 32);
        assert_eq!(core::mem::size_of::<Event<'_>>(), 48);
    }
}

#[test]
fn node_info_round_trips_compact_and_wide() {
    let header = BlockHeader::new(Chomping::Strip, Some(3), 7, Span::new(20, 23));
    let props = Properties::new(Some(10), Some(14), Some(2));
    let info = NodeInfo::new(30, props, Some(header));
    assert!(info.is_compact());
    assert_eq!(info.properties(30), props);
    assert_eq!(info.block_header(30), Some(header));
    assert!(!info.has_no_properties());

    let none = NodeInfo::new(30, Properties::none(), None);
    assert_eq!(none, NodeInfo::none());
    assert_eq!(NodeInfo::default(), NodeInfo::none());
    assert!(none.has_no_properties());
    assert_eq!(none.block_header(30), None);

    // What does not fit the compact form keeps its meaning: a property at
    // or after the node's start, an indicator of 0, a long header, or (past
    // 4 GiB) an offset that needs more than 32 bits.
    let odd = [
        (Properties::new(Some(30), None, None), None),
        (Properties::new(None, Some(31), None), None),
        (
            Properties::none(),
            Some(BlockHeader::new(
                Chomping::Keep,
                Some(0),
                1,
                Span::new(1, 2),
            )),
        ),
        (
            Properties::none(),
            Some(BlockHeader::new(Chomping::Keep, None, 1, Span::new(0, 300))),
        ),
        (
            Properties::none(),
            Some(BlockHeader::new(Chomping::Clip, None, 1, Span::new(5, 1))),
        ),
    ];
    for (props, block) in odd {
        let info = NodeInfo::new(30, props, block);
        assert!(!info.is_compact());
        assert_eq!(info.properties(30), props);
        assert_eq!(info.block_header(30), block);
    }
    if let Ok(far) = usize::try_from(5_000_000_000u64) {
        let props = Properties::new(Some(1), Some(far - 3), Some(0));
        let header = BlockHeader::new(Chomping::Clip, None, far, Span::new(far - 2, far - 1));
        let info = NodeInfo::new(far, props, Some(header));
        assert!(!info.is_compact());
        assert_eq!(info.properties(far), props);
        assert_eq!(info.block_header(far), Some(header));
        let event = Event::scalar(
            Span::new(far, far),
            ScalarStyle::Literal,
            props,
            Some(header),
        );
        assert_eq!(event.properties(), Some(props));
        assert_eq!(event.block_header(), Some(header));
        // Comparing and cloning the wide form (it owns a box) works.
        assert_eq!(event.clone(), event);
    }
}

#[test]
fn readers_take_meaning_from_the_source() {
    let src = "%TAG !e! tag:e.com,2000:\n--- &a !e!x%21 'it''s'\n";
    let events = crate::parse(src);
    let d = events
        .iter()
        .find(|e| matches!(e.kind(), EventKind::Directive(_)))
        .unwrap();
    assert_eq!(
        d.directive(src),
        Some(DirectiveKind::tag("!e!", "tag:e.com,2000:"))
    );
    // Another text at the same span is not a directive.
    assert_eq!(d.directive("xxxxxxxxxxxxxxxxxxxxxxxxxxxx"), None);
    let s = events.iter().find(|e| e.value(src).is_some()).unwrap();
    assert_eq!(s.value(src).unwrap(), "it's");
    assert_eq!(s.anchor(src).unwrap().name, "a");
    let tag = s.tag(src).unwrap();
    assert_eq!(
        (tag.handle, tag.suffix, tag.resolved.as_ref()),
        ("!e!", "x%21", "tag:e.com,2000:x!")
    );
    // Positions that do not point at a property read as nothing.
    let wrong = Event::scalar(
        Span::new(10, 12),
        ScalarStyle::Plain,
        Properties::new(Some(1), Some(2), None),
        None,
    );
    assert!(wrong.anchor(src).is_none());
    assert!(wrong.tag(src).is_none());
    let alias = Event::new(EventKind::Alias, Span::new(0, 3));
    assert_eq!(alias.alias_name("*ab"), Some("ab"));
    assert_eq!(alias.alias_name("&ab"), None);
}
