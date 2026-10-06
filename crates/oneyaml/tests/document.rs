//! `Document` and `Stream`: the read-only lossless view (a byte-identical
//! round trip, one interpretation).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{
    Document, EditError, NodeKind, Path, ScalarStyle, Settings, Stream, Value, code, from_str, path,
};

const K8S: &str = "\
# Deployment
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web   # the name
  labels: {app: web, tier: \"front\"}
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: nginx
          image: 'nginx:1.27'
          ports: [80, 443]
";

#[test]
fn untouched_documents_emit_their_input() {
    for src in [
        K8S,
        "",
        "a",
        "# c\n",
        "--- |\n  x\n...\n",
        "a: 1\r\nb: 2\r\n",
    ] {
        let doc = Document::parse(src).unwrap();
        assert_eq!(doc.to_string().as_bytes(), src.as_bytes());
        assert_eq!(doc.to_bytes(), src.as_bytes());
    }
}

#[test]
fn navigation_views_and_spans() {
    let doc = Document::parse(K8S).unwrap();
    let root = doc.root().unwrap();
    assert_eq!(root.kind(), NodeKind::Mapping);
    assert_eq!(root.as_mapping().unwrap().len(), 4);
    let image = doc
        .get_path(&path!["spec", "template", "spec", "containers", 0, "image"])
        .unwrap();
    let s = image.as_scalar().unwrap();
    assert_eq!(s.value(), "nginx:1.27");
    assert_eq!(s.style(), ScalarStyle::SingleQuoted);
    assert_eq!(image.text(), "'nginx:1.27'");
    assert_eq!(&K8S[image.span().start..image.span().end], "'nginx:1.27'");
    assert_eq!(s.resolved().unwrap(), Value::from("nginx:1.27"));
    let replicas = doc.get("spec").unwrap().get("replicas").unwrap();
    assert_eq!(
        replicas.as_scalar().unwrap().resolved().unwrap(),
        Value::from(3)
    );
    let tier = doc.get_path(&path!["metadata", "labels", "tier"]).unwrap();
    assert_eq!(tier.as_scalar().unwrap().style(), ScalarStyle::DoubleQuoted);
    let ports = doc.get_path(&path!["spec", "template", "spec", "containers", 0, "ports"]);
    let ports = ports.unwrap().as_sequence().unwrap();
    assert_eq!(ports.len(), 2);
    assert!(!ports.is_empty());
    assert_eq!(ports.get(1).unwrap().text(), "443");
    assert_eq!(ports.iter().count(), 2);
    assert!(ports.get(2).is_none());
    // Index: the meaning, never panics.
    assert_eq!(
        doc.to_value().unwrap()["spec"]["replicas"].as_u64(),
        Some(3)
    );
    assert_eq!(
        doc.to_value().unwrap()["spec"]["template"]["spec"]["containers"][0]["name"],
        Value::from("nginx")
    );
    assert_eq!(doc.to_value().unwrap()["nope"]["deeper"][7], Value::Null);
    assert_eq!(doc.to_value().unwrap()[0], Value::Null);
}

#[test]
fn path_errors_are_typed() {
    let doc = Document::parse("a: [1, {b: 2}]\ns: x\n").unwrap();
    assert!(matches!(
        doc.clone().get_path_mut(&path!["zz"]).unwrap_err(),
        EditError::NotFound { ref at, .. } if *at == path!["zz"]
    ));
    assert!(matches!(
        doc.clone().get_path_mut(&path!["s", "k"]).unwrap_err(),
        EditError::NotAMapping { ref at, .. } if *at == path!["s"]
    ));
    assert!(matches!(
        doc.clone().get_path_mut(&path!["s", 0]).unwrap_err(),
        EditError::NotASequence { ref at, .. } if *at == path!["s"]
    ));
    let e = doc.clone().get_path_mut(&path!["a", 5]).unwrap_err();
    assert!(matches!(
        e,
        EditError::IndexOutOfBounds { ref at, index: 5, len: 2, .. } if *at == path!["a"]
    ));
    assert_eq!(e.to_string(), "index 5 out of bounds at a (length 2)");
    assert_eq!(doc.get_path(&path!["a", 1, "b"]).unwrap().text(), "2");
    assert_eq!(doc.get_path(Path::new()).unwrap(), doc.root().unwrap());
    let empty = Document::parse("").unwrap();
    assert!(empty.root().is_none());
    assert!(matches!(
        empty.clone().get_path_mut(Path::new()).unwrap_err(),
        EditError::NotFound { ref at, .. } if *at == Path::new()
    ));
    // Every variant displays (the struct-like ones are `#[non_exhaustive]`:
    // made by the walk, not by hand).
    let bad = Document::parse_recovering("a: 'x\n").into_value();
    for e in [
        EditError::StaleNode,
        doc.clone().get_path_mut(&path!["x"]).unwrap_err(),
        doc.clone().get_path_mut(&path!["s", "x"]).unwrap_err(),
        doc.clone().get_path_mut(&path!["s", 0]).unwrap_err(),
        bad.clone().get_path_mut(&path!["a", "k"]).unwrap_err(),
    ] {
        assert!(!e.to_string().is_empty());
        assert_eq!(e.at().is_none(), e == EditError::StaleNode);
    }
}

#[test]
fn keys_match_by_meaning_last_wins_and_merges() {
    let src = "1: int\n'1': str\nb: first\nb: last\n";
    let doc = Document::parse_with(
        src,
        &Settings::new().duplicate_keys(oneyaml::DuplicateKeys::LastWins),
    )
    .unwrap();
    assert_eq!(doc.get("1").unwrap().text(), "str");
    assert_eq!(doc.get("b").unwrap().text(), "last");
    let merged = "base: &b {x: 1, y: 2}\nmore: &m {z: 3}\nuse:\n  <<: [*b, *m]\n  y: own\n";
    let s = Settings::new().merge_keys(true);
    let doc = Document::parse_with(merged, &s).unwrap();
    let u = doc.get("use").unwrap();
    assert_eq!(u.get("x").unwrap().text(), "1");
    assert_eq!(u.get("y").unwrap().text(), "own");
    assert_eq!(u.get("z").unwrap().text(), "3");
    assert!(u.get("w").is_none());
    assert_eq!(doc.to_value().unwrap()["use"]["z"].as_u64(), Some(3));
    // Without merge keys `<<` is an ordinary key.
    let doc = Document::parse(merged).unwrap();
    assert!(doc.get("use").unwrap().get("x").is_none());
    assert!(doc.get("use").unwrap().get("<<").is_some());
}

#[test]
fn anchors_aliases_tags() {
    let doc = Document::parse("a: &x !!str 1\nb: *x\nc: !custom [1]\n--- \n").err();
    assert_eq!(doc.unwrap().code(), code::MULTIPLE_DOCUMENTS);
    // An undefined alias fails strict parsing with serde's Y1015.
    let undefined = "a: &x !!str 1\nb: *x\nc: !custom [1]\nd: *nope\n";
    let e = Document::parse(undefined).unwrap_err();
    assert_eq!(e.code(), code::UNDEFINED_ALIAS);
    assert_eq!(e.code(), from_str::<Value>(undefined).unwrap_err().code());
    // A recovering parse keeps the document, with the error as a
    // diagnostic.
    let p = Document::parse_recovering(undefined);
    assert!(
        p.diagnostics()
            .iter()
            .any(|d| d.code == code::UNDEFINED_ALIAS)
    );
    let doc = p.into_value();
    let a = doc.get("a").unwrap();
    assert_eq!(a.anchor(), Some("x"));
    assert_eq!(
        a.tag().unwrap().map(|t| t.to_string()).as_deref(),
        Some("tag:yaml.org,2002:str")
    );
    let b = doc.get("b").unwrap();
    assert_eq!(b.kind(), NodeKind::Alias);
    assert_eq!(b.alias_name(), Some("x"));
    assert_eq!(b.alias_target(), Some(a));
    assert_eq!(b.as_scalar().unwrap().value(), "1");
    assert_eq!(b.resolved(), a);
    assert_eq!(
        doc.get("c")
            .unwrap()
            .tag()
            .unwrap()
            .map(|t| t.to_string())
            .as_deref(),
        Some("!custom")
    );
    assert_eq!(doc.get("d").unwrap().alias_target(), None);
    assert!(a.alias_target().is_none());
    // The meaning of an undefined alias is an error, as for serde.
    let e = doc.to_value().unwrap_err();
    let f = from_str::<Value>("a: &x !!str 1\nb: *x\nc: !custom [1]\nd: *nope\n").unwrap_err();
    assert_eq!(e.code(), f.code());
}

#[test]
fn handles_resolve_and_go_stale() {
    let doc = Document::parse(K8S).unwrap();
    let id = doc.id_of(&path!["spec", "replicas"]).unwrap();
    assert_eq!(doc.node(id).unwrap().text(), "3");
    let other = Document::parse(K8S).unwrap();
    assert_eq!(other.node(id).unwrap_err(), EditError::StaleNode);
    let copy = doc.clone();
    assert_eq!(copy.node(id).unwrap().text(), "3");
    assert!(doc.id_of(&path!["zz"]).is_none());
}

#[test]
fn restricted_documents_expose_error_nodes() {
    let p = Document::parse_recovering("a: [1, 2\nb: 3\n");
    assert!(p.has_errors());
    let doc = p.value();
    assert_eq!(doc.to_string(), "a: [1, 2\nb: 3\n");
    assert!(doc.has_errors());
    let errors: Vec<_> = doc
        .syntax()
        .root()
        .descendants()
        .filter(|n| n.kind() == oneyaml_syntax::tree::SyntaxKind::Error)
        .collect();
    assert!(!errors.is_empty());
    // Strict parse fails with the same code the serde path reports.
    let strict = Document::parse("a: [1, 2\nb: 3\n").unwrap_err();
    let serde = from_str::<Value>("a: [1, 2\nb: 3\n").unwrap_err();
    assert_eq!(strict.code(), serde.code());
    assert_eq!(doc.to_value().unwrap_err().code(), serde.code());
    // Error regions are read-only error nodes, met where they broke a node.
    let p = Document::parse_recovering("a: 'x\n");
    let doc = p.value();
    let a = doc.get("a").unwrap();
    assert_eq!(a.kind(), NodeKind::Error);
    assert_eq!(a.text(), "'x");
    assert_eq!(
        a.diagnostic().unwrap().code,
        code::UNTERMINATED_QUOTED_SCALAR
    );
    assert!(a.as_scalar().is_none() && a.get("k").is_none());
    assert!(matches!(
        doc.clone().get_path_mut(&path!["a", "k"]).unwrap_err(),
        EditError::ErrorNode { ref at, .. } if *at == path!["a"]
    ));
    assert_eq!(doc.errors().count(), 1);
    assert_eq!(doc.errors().next().unwrap(), a);
    assert_eq!(doc.node(a.id()).unwrap(), a);
    let doc = Document::parse_recovering("]\n").into_value();
    assert!(doc.has_errors() && doc.errors().all(|e| e.diagnostic().is_some()));
    assert_eq!(Document::parse(K8S).unwrap().errors().count(), 0);
}

#[test]
fn settings_and_diagnostics_are_recorded() {
    let s = Settings::new().merge_keys(true);
    let doc = Document::parse_with("%YAML 1.1\n--- a\n", &s).unwrap();
    assert_eq!(doc.settings(), &s);
    let d = doc.diagnostics();
    assert!(d.iter().any(|d| d.severity == oneyaml::Severity::Warning));
    assert!(!doc.has_errors());
}

#[test]
fn utf16_and_utf32_round_trip_their_bytes() {
    let text = "\u{feff}a: [é, 2]\n";
    for enc in [
        oneyaml::Encoding::Utf16Le,
        oneyaml::Encoding::Utf16Be,
        oneyaml::Encoding::Utf32Le,
        oneyaml::Encoding::Utf32Be,
    ] {
        let bytes = oneyaml_syntax::encode(text, enc).unwrap();
        let doc = Document::from_slice(&bytes).unwrap();
        assert_eq!(doc.encoding(), enc);
        assert_eq!(doc.to_bytes(), bytes);
        assert_eq!(doc.to_string(), text);
        assert_eq!(doc.to_value().unwrap()["a"][0], Value::from("é"));
    }
    // Invalid bytes: strict fails with the decoder's code, recovery keeps
    // the bytes.
    let bad = b"a: \xff\n";
    let e = Document::from_slice(bad).unwrap_err();
    assert_eq!(
        e.code(),
        oneyaml::from_slice::<Value>(bad).unwrap_err().code()
    );
    let p = Document::from_slice_recovering(bad, &Settings::new());
    assert!(p.has_errors());
    assert_eq!(p.value().to_bytes(), bad);
    assert_eq!(p.value().to_value().unwrap_err().code(), e.code());
}

#[test]
fn streams_hold_every_document() {
    let src = "a: 1\n---\nb: 2\n...\n# tail\n--- [3]\n";
    let stream = Stream::parse(src).unwrap();
    assert_eq!(stream.len(), 3);
    assert!(!stream.is_empty());
    assert_eq!(stream.to_string(), src);
    assert_eq!(stream.to_bytes(), src.as_bytes());
    let docs: Vec<Document> = stream.documents().collect();
    assert_eq!(docs[1].to_string(), "---\nb: 2\n...\n"); // its region
    assert_eq!(docs[1].get("b").unwrap().text(), "2");
    assert_eq!(
        docs[2].to_value().unwrap(),
        Value::from(vec![Value::from(3)])
    );
    // Each document's value equals the serde stream's.
    let serde: Vec<Value> = oneyaml::Deserializer::from_str(src)
        .map(|d| serde::Deserialize::deserialize(d).unwrap())
        .collect();
    let ours: Vec<Value> = docs.iter().map(|d| d.to_value().unwrap()).collect();
    assert_eq!(ours, serde);
    // Handles are per document.
    let id = docs[0].id_of(&path!["a"]).unwrap();
    assert!(docs[0].node(id).is_ok());
    assert_eq!(docs[1].node(id).unwrap_err(), EditError::StaleNode);
    assert!(stream.document(3).is_none());
    assert_eq!(stream.settings(), &Settings::new());
    assert!(stream.diagnostics().is_empty());
    assert!(stream.syntax().emit() == src);
    // Recovering and strict.
    assert!(Stream::parse("a\n--- [\n").is_err());
    let p = Stream::parse_recovering("a\n--- [\n");
    assert!(p.has_errors());
    assert_eq!(p.value().len(), 2);
    let empty = Stream::parse("").unwrap();
    assert!(empty.is_empty());
    let utf16 = oneyaml_syntax::encode("a\n--- b\n", oneyaml::Encoding::Utf16Le).unwrap();
    let s = Stream::from_slice_with(&utf16, &Settings::new()).unwrap();
    assert_eq!(s.to_bytes(), utf16);
    assert!(
        !Stream::from_slice_recovering(b"\xff", &Settings::new())
            .diagnostics()
            .is_empty()
    );
    // A whole document over a multi-document input reports it.
    let p = Document::parse_recovering(src);
    assert!(
        p.diagnostics()
            .iter()
            .any(|d| d.code == code::MULTIPLE_DOCUMENTS)
    );
}

#[test]
fn value_try_from_matches_serde() {
    // A recursive alias no longer
    // parses strictly; it is serde's Y1015.
    assert_eq!(
        Document::parse("&a [1, *a]").unwrap_err().code(),
        from_str::<Value>("&a [1, *a]").unwrap_err().code()
    );
    for src in [K8S, "", "{? [1]: 2}", "!!int 08"] {
        let doc = Document::parse(src).unwrap();
        let ours = Value::try_from(&doc).map_err(|e| e.code());
        let serde = from_str::<Value>(src).map_err(|e| e.code());
        assert_eq!(ours, serde, "{src:?}");
    }
    // Strict parsing checks the meaning
    // too, without expansion, with serde's first error code.
    for src in [
        "!!int x",
        "a: 1\na: 2\n",
        "!!map x",
        "a: [!!int x]\n",
        "{? [1]: 2, ? [1]: 3}",
        "a: &x 1\nb: {*x : 1, 1: 2}\n",
    ] {
        let ours = Document::parse(src).unwrap_err().code();
        let serde = from_str::<Value>(src).unwrap_err().code();
        assert_eq!(ours, serde, "{src:?}");
        let p = Document::parse_recovering(src);
        assert!(p.diagnostics().iter().any(|d| d.code == serde), "{src:?}");
        assert!(p.value().to_value().is_err());
    }
    // Settings diagnostics are recorded as for serde.
    let meaningless = Settings::new()
        .schema(oneyaml::Schema::Json)
        .yaml11_booleans(true);
    let doc = Document::parse_with("1", &meaningless).unwrap();
    assert!(
        doc.diagnostics()
            .iter()
            .any(|d| d.code == code::MEANINGLESS_SETTINGS)
    );
    let (_, w) = oneyaml::from_str_with_diagnostics::<Value>("1", &meaningless).unwrap();
    assert!(w.iter().any(|d| d.code == code::MEANINGLESS_SETTINGS));
}

#[test]
fn deep_documents_on_a_small_stack() {
    // Parse, emit, navigate, and convert without recursion.
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let n = 100_000;
            let src = format!("{}{}", "[".repeat(n), "]".repeat(n));
            let limits = oneyaml::Limits::unlimited();
            let s = Settings::new().limits(limits);
            let doc = Document::parse_with(src.as_str(), &s).unwrap();
            assert_eq!(doc.to_string().len(), src.len());
            let v = doc.to_value().unwrap();
            assert_eq!(v.depth(), n);
            let mut node = doc.root().unwrap();
            for _ in 0..10 {
                node = node.get_index(0).unwrap();
            }
            assert_eq!(node.kind(), NodeKind::Sequence);
            // Default limits: an error, never a crash.
            let p = Document::parse_recovering(src.as_str());
            assert!(p.has_errors());
            assert!(Document::parse(src.as_str()).is_err());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn strict_parse_checks_meaning_without_expansion() {
    // Strict parsing reports meaning errors in
    // source order, with serde's codes; merge values too.
    let merge = Settings::new().merge_keys(true);
    for (src, s) in [
        ("a: {<<: 1}\n", &merge),
        ("a: {<<: [{}, 2]}\n", &merge),
        ("a: {<<: {}, <<: {}}\n", &merge),
        ("b: &s x\na: {<<: *s}\n", &merge),
        ("a: !!int x\nb: [\n", &Settings::new()),
        ("x: !!str [1]\n", &Settings::new()),
    ] {
        let ours = Document::parse_with(src, s).unwrap_err().code();
        let serde = oneyaml::from_str_with::<Value>(src, s).unwrap_err().code();
        assert_eq!(ours, serde, "{src:?}");
        assert_eq!(
            Stream::parse_with(src, s).unwrap_err().code(),
            serde,
            "{src:?}"
        );
    }
    // Aliases are not expanded: an alias stands for its anchor's shape.
    let ok = "base: &b {x: 1}\nlist: &l [{y: 2}]\nu: {<<: *b}\nv: {<<: *l}\nw: {<<: [*b]}\n";
    assert!(Document::parse_with(ok, &merge).is_ok());
    // Undefined and recursive aliases need no expansion to find:
    // strict parsing fails with serde's Y1015, in source order (an earlier
    // meaning error wins, a later one does not).
    for src in [
        "a: *nope\n",
        "&a [1, *a]\n",
        "&a {x: *a}\n",
        "a: &a [&b 1, *a]\n",
        "a: &x 1\n--- \nb: *x\n",
        "a: *nope\nb: !!int x\n",
        "{&a [*a]: 1}\n",
    ] {
        let serde = oneyaml::Deserializer::from_str(src)
            .map(<Value as serde::Deserialize>::deserialize)
            .find_map(Result::err)
            .unwrap();
        assert_eq!(serde.code(), code::UNDEFINED_ALIAS, "{src:?}");
        let e = Stream::parse(src).unwrap_err();
        assert_eq!(e.code(), code::UNDEFINED_ALIAS, "{src:?}");
        // The same message and span (serde adds the path it was at).
        assert_eq!(
            e.diagnostic().message,
            serde.diagnostic().message,
            "{src:?}"
        );
        assert_eq!(e.span(), serde.span(), "{src:?}");
        if !src.contains("---") {
            let e = Document::parse(src).unwrap_err();
            assert_eq!(e.code(), code::UNDEFINED_ALIAS, "{src:?}");
        }
        let p = Stream::parse_recovering(src);
        assert!(
            p.diagnostics()
                .iter()
                .any(|d| d.code == code::UNDEFINED_ALIAS),
            "{src:?}"
        );
    }
    // An earlier meaning error is reported first.
    let src = "a: !!int x\nb: *nope\n";
    assert_eq!(
        Document::parse(src).unwrap_err().code(),
        from_str::<Value>(src).unwrap_err().code()
    );
    assert_ne!(
        Document::parse(src).unwrap_err().code(),
        code::UNDEFINED_ALIAS
    );
    // Anchors are per document; a redefinition inside an anchored node
    // is what later aliases mean (§3.2.2.2), for the shape a `<<` needs.
    let merge = Settings::new().merge_keys(true);
    for src in [
        "- &a {x: &a 1}\n- {<<: *a}\n",
        "- &a [&a {x: 1}]\n- {<<: *a}\n",
        "a: &a [&a 1, *a]\n",
    ] {
        let ours = Document::parse_with(src, &merge).map_err(|e| e.code());
        let serde = oneyaml::from_str_with::<Value>(src, &merge).map_err(|e| e.code());
        match (&ours, &serde) {
            (Ok(doc), Ok(v)) => assert_eq!(&doc.to_value().unwrap(), v, "{src:?}"),
            (Err(a), Err(b)) => assert_eq!(a, b, "{src:?}"),
            _ => panic!("{src:?}: {ours:?} vs {serde:?}"),
        }
    }
    // An alias key means its anchor's value (within a copy
    // budget, no expansion beyond it): the parse finds this duplicate, as
    // serde does.
    let src = "{&a [x]: 1, *a : 2}\n";
    let e = Document::parse(src).unwrap_err();
    let serde = oneyaml::from_str::<Value>(src).unwrap_err();
    assert_eq!((e.code(), e.span()), (serde.code(), serde.span()));
    // An alias bomb parses (no expansion); its meaning hits the budget.
    let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for i in 1..12 {
        let prev = format!("*a{}", i - 1);
        bomb.push_str(&format!("a{i}: &a{i} [{}]\n", vec![prev; 10].join(", ")));
    }
    let doc = Document::parse(bomb.as_str()).unwrap();
    assert_eq!(
        doc.to_value().unwrap_err().code(),
        code::ALIAS_BUDGET_EXCEEDED
    );
    // LastWins duplicates are warnings the document keeps.
    let lw = Settings::new().duplicate_keys(oneyaml::DuplicateKeys::LastWins);
    let doc = Document::parse_with("a: 1\na: 2\n", &lw).unwrap();
    assert!(
        doc.diagnostics()
            .iter()
            .any(|d| d.code == code::DUPLICATE_KEY_LAST_WINS)
    );
    // Recovering: meaning errors are diagnostics, one per document.
    let p = Stream::parse_recovering("a: !!int x\n---\nb: 1\nb: 2\n");
    let codes: Vec<_> = p
        .diagnostics()
        .iter()
        .filter(|d| d.severity == oneyaml::Severity::Error)
        .map(|d| d.code)
        .collect();
    assert_eq!(codes.len(), 2, "{codes:?}");
    assert_eq!(codes[1], code::DUPLICATE_KEY);
}
