//! Names and values are read only through `Sourced`, which the
//! parser pairs with the event's own source; the pulled and the collected
//! streams read the same; values still borrow from the input.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::borrow::Cow;

use oneyaml_syntax::event::{DirectiveKind, EventKind};
use oneyaml_syntax::{EventList, Sourced, events, parse};

const SRC: &str = "%TAG !e! tag:e.com,2000:\n--- &a !e!x%21 {k: 'it''s', p: plain, al: *a}\n";

/// Everything a reader can say about one event.
fn reads<E: std::borrow::Borrow<oneyaml_syntax::Event<'static>>>(
    e: &Sourced<'static, E>,
) -> String {
    format!(
        "{:?} {:?} {:?} {:?} {:?} {:?}",
        e.text(),
        e.value(),
        e.anchor().map(|a| a.name),
        e.tag().map(|t| t.resolved),
        e.alias_name(),
        e.directive(),
    )
}

#[test]
fn pulled_collected_and_owned_reads_agree() {
    let list: EventList<'static> = parse(SRC);
    assert!(std::ptr::eq(list.source(), SRC));
    let pulled: Vec<String> = events(SRC).map(|e| reads(&e)).collect();
    let viewed: Vec<String> = list.sourced().map(|e| reads(&e)).collect();
    let by_index: Vec<String> = (0..list.len())
        .map(|i| reads(&list.get_sourced(i).unwrap()))
        .collect();
    let owned: Vec<String> = list.clone().into_iter().map(|e| reads(&e)).collect();
    assert_eq!(pulled, viewed);
    assert_eq!(pulled, by_index);
    assert_eq!(pulled, owned);
    assert!(list.get_sourced(list.len()).is_none());
    assert_eq!(list.sourced().len(), list.len());
    assert_eq!(
        list.sourced().rev().map(|e| reads(&e)).collect::<Vec<_>>(),
        viewed.iter().rev().cloned().collect::<Vec<_>>()
    );
}

#[test]
fn readers_resolve_names_and_values() {
    let list = parse(SRC);
    let d = list
        .sourced()
        .find(|e| matches!(e.kind(), EventKind::Directive(_)))
        .unwrap();
    assert_eq!(
        d.directive(),
        Some(DirectiveKind::tag("!e!", "tag:e.com,2000:"))
    );
    let map = list
        .sourced()
        .find(|e| matches!(e.kind(), EventKind::MappingStart { .. }))
        .unwrap();
    assert_eq!(map.anchor().unwrap().name, "a");
    assert_eq!(map.tag().unwrap().resolved, "tag:e.com,2000:x!");
    let values: Vec<Cow<'_, str>> = list.sourced().filter_map(|e| e.value()).collect();
    assert_eq!(values, ["k", "it's", "p", "plain", "al"]);
    let alias = list.sourced().find_map(|e| e.alias_name()).unwrap();
    assert_eq!(alias, "a");
    // Readers answer `None` for events of another kind.
    let start = list.get_sourced(0).unwrap();
    assert!(matches!(start.kind(), EventKind::StreamStart));
    assert_eq!(start.text(), Some(""));
    assert!(start.value().is_none() && start.tag().is_none());
}

/// A value that is a slice of the input borrows from the input, not
/// from the `Sourced` or the list, so it outlives both.
#[test]
fn values_borrow_from_the_source() {
    let src = String::from("k: plain\n");
    let value = {
        let pulled = events(&src).find(|e| e.text() == Some("plain")).unwrap();
        pulled.value().unwrap()
    };
    let Cow::Borrowed(v) = value else {
        panic!("a plain value is borrowed")
    };
    let range = src.as_bytes().as_ptr_range();
    assert!(range.contains(&v.as_ptr()));
    let value = {
        let list = parse(&src);
        let at = list
            .sourced()
            .position(|e| e.text() == Some("plain"))
            .unwrap();
        list.get_sourced(at).unwrap().value().unwrap()
    };
    assert!(matches!(value, Cow::Borrowed("plain")));
}
