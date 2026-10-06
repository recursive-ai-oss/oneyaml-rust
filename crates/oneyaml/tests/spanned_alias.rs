//! `Spanned` through aliases. A node reached through an alias
//! spans the alias (`*x`), start and end, for scalars, sequences, and
//! mappings; every span is well-formed and slices the source.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use oneyaml::{Settings, Spanned, Value, code, from_str, from_str_with};
use serde::Deserialize;

/// The text `sp` covers, asserting it is well-formed (slicing never
/// panics).
fn text<T>(src: &str, sp: &Spanned<T>) -> String {
    let span = sp.span();
    assert!(
        span.start <= span.end,
        "inverted {}..{}",
        span.start,
        span.end
    );
    src.get(span.start..span.end)
        .expect("span inside the source, on char boundaries")
        .to_owned()
}

#[derive(Debug, Deserialize)]
struct B<T> {
    b: Spanned<T>,
}

#[test]
fn mapping_alias_spans_the_alias() {
    let src = "a: &x {k: 1}\nb: *x\n";
    let v: B<BTreeMap<String, u8>> = from_str(src).unwrap();
    assert_eq!((v.b.span().start, v.b.span().end), (16, 18));
    assert_eq!(text(src, &v.b), "*x");
    assert_eq!(v.b.get_ref().get("k"), Some(&1));
}

#[test]
fn sequence_alias_spans_the_alias() {
    let src = "p: 0\nq: &z [1,2]\nr: *z";
    #[derive(Deserialize)]
    struct R {
        r: Spanned<Vec<u8>>,
    }
    let v: R = from_str(src).unwrap();
    assert_eq!(text(src, &v.r), "*z");
    assert_eq!(*v.r, vec![1, 2]);
    // Block sequence anchor too.
    let src = "q: &z\n  - 1\n  - 2\nr: *z\n";
    let v: R = from_str(src).unwrap();
    assert_eq!(text(src, &v.r), "*z");
    assert_eq!(v.r.span().start, src.find("*z").unwrap());
}

#[test]
fn scalar_alias_spans_the_alias() {
    let src = "a: &s hello\nb: *s\n";
    let v: B<String> = from_str(src).unwrap();
    assert_eq!(text(src, &v.b), "*s");
    assert_eq!(v.b.as_str(), "hello");
}

#[test]
fn anchored_node_keeps_its_own_span_and_inner_nodes_theirs() {
    #[derive(Deserialize)]
    struct Inner {
        k: Spanned<u8>,
    }
    #[derive(Deserialize)]
    struct Ab {
        a: Spanned<Inner>,
        b: Spanned<Inner>,
    }
    let src = "a: &x {k: 1}\nb: *x\n";
    let v: Ab = from_str(src).unwrap();
    assert_eq!(text(src, &v.a), "{k: 1}");
    assert_eq!(text(src, &v.b), "*x");
    // Inside the aliased mapping, nodes keep their anchored spans.
    assert_eq!(text(src, &v.a.k), "1");
    assert_eq!(text(src, &v.b.k), "1");
}

#[test]
fn nested_aliases() {
    // `*y` replays `[*x, *x]`, whose items replay `{k: 1}`.
    let src = "x: &x {k: 1}\ny: &y [*x, *x]\nz: *y\n";
    #[derive(Deserialize)]
    struct Z {
        y: Spanned<Vec<Spanned<BTreeMap<String, u8>>>>,
        z: Spanned<Vec<Spanned<BTreeMap<String, u8>>>>,
    }
    let v: Z = from_str(src).unwrap();
    assert_eq!(text(src, &v.y), "[*x, *x]");
    assert_eq!(text(src, &v.z), "*y");
    for item in v.y.iter().chain(v.z.iter()) {
        assert_eq!(text(src, item), "*x");
        assert_eq!(item.get_ref().get("k"), Some(&1));
    }
    // The items of `y` point at their own aliases.
    let first = src.find("*x").unwrap();
    assert_eq!(v.y[0].span().start, first);
    assert_eq!(
        v.y[1].span().start,
        src[first + 1..].find("*x").unwrap() + first + 1
    );
    // The last item is the anchored node's last event: still the alias.
    let src = "x: &x [1]\ny: &y [0, *x]\nz: *y\n";
    #[derive(Deserialize)]
    struct Z2 {
        z: Spanned<Vec<Spanned<Value>>>,
    }
    let v: Z2 = from_str(src).unwrap();
    assert_eq!(text(src, &v.z), "*y");
    assert_eq!(text(src, &v.z[0]), "0");
    assert_eq!(text(src, &v.z[1]), "*x");
    // Scalar anchored inside an anchored collection, aliased twice.
    let src = "a: &o [&i 5]\nb: *o\nc: *i\n";
    #[derive(Deserialize)]
    struct Bc {
        b: Spanned<Vec<Spanned<u8>>>,
        c: Spanned<u8>,
    }
    let v: Bc = from_str(src).unwrap();
    assert_eq!(text(src, &v.b), "*o");
    assert_eq!(text(src, &v.b[0]), "5");
    assert_eq!(text(src, &v.c), "*i");
}

#[test]
fn aliases_inside_merges() {
    let merge = Settings::new().merge_keys(true);
    #[derive(Debug, Deserialize)]
    struct M {
        m: Spanned<BTreeMap<String, Spanned<Value>>>,
        n: Spanned<BTreeMap<String, Spanned<Value>>>,
    }
    let src = "base: &b {k: &v [1, 2], j: 3}\nm:\n  <<: *b\n  own: *v\nn: *m\n";
    // `*m` is undefined (no anchor `m`): an error, with a well-formed span.
    let e = from_str_with::<M>(src, &merge).unwrap_err();
    assert_eq!(e.code(), code::UNDEFINED_ALIAS);
    let sp = e.diagnostic().primary.span;
    assert_eq!(&src[sp.start..sp.end], "*m");

    let src = "base: &b {k: &v [1, 2], j: 3}\nm: &m\n  <<: *b\n  own: *v\nn: *m\n";
    let v: M = from_str_with(src, &merge).unwrap();
    let m_text = text(src, &v.m);
    assert!(m_text.starts_with("<<: *b"), "{m_text:?}");
    assert_eq!(text(src, &v.n), "*m");
    for map in [&v.m, &v.n] {
        // Merged entries keep the anchored spans of their source.
        assert_eq!(text(src, &map["k"]), "[1, 2]");
        assert_eq!(text(src, &map["j"]), "3");
        assert_eq!(text(src, &map["own"]), "*v");
    }
    // A merge source that is itself an alias inside a `<<` sequence.
    let src = "a: &a {x: 1}\nb: &b {y: 2}\nc:\n  <<: [*a, *b]\n  z: *a\n";
    #[derive(Deserialize)]
    struct C {
        c: Spanned<BTreeMap<String, Spanned<Value>>>,
    }
    let v: C = from_str_with(src, &merge).unwrap();
    assert_eq!(text(src, &v.c.get_ref()["x"]), "1");
    assert_eq!(text(src, &v.c.get_ref()["y"]), "2");
    assert_eq!(text(src, &v.c.get_ref()["z"]), "*a");
    text(src, &v.c);
}

#[test]
fn errors_through_aliases_have_well_formed_spans() {
    // A type error on an aliased mapping points at the alias.
    let src = "a: &x {k: 1}\nb: *x\n";
    let e = from_str::<B<u8>>(src).unwrap_err();
    let sp = e.diagnostic().primary.span;
    assert!(sp.start <= sp.end);
    assert_eq!(&src[sp.start..sp.end], "*x");
    // A type error inside an aliased mapping points into the anchor.
    let e = from_str::<B<BTreeMap<String, bool>>>(src).unwrap_err();
    let sp = e.diagnostic().primary.span;
    assert!(sp.start <= sp.end && sp.end <= src.len());
    let _ = &src[sp.start..sp.end];
    // Duplicate keys through aliases: every label slices.
    let src = "k: &k a\nm: {*k : 1, a: 2}\n";
    let e = from_str::<Value>(src).unwrap_err();
    for l in std::iter::once(&e.diagnostic().primary).chain(&e.diagnostic().secondary) {
        assert!(l.span.start <= l.span.end);
        let _ = &src[l.span.start..l.span.end];
    }
}
