//! The happy path — string paths everywhere a `Path` is taken,
//! and edits that take values as well as references.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, EditError, Path, Stream, Value, path};
use serde::Serialize;

const SRC: &str = "\
# app config
name: web # public name
spec:
  ports: [80, 443]
  labels:
    app.kubernetes.io/name: web
  env:
    - LOG=info
";

#[test]
fn string_paths_read_as_path_macro() {
    let doc = Document::parse(SRC).unwrap();
    for (s, p) in [
        ("name", path!["name"]),
        ("spec.ports[1]", path!["spec", "ports", 1]),
        (
            "spec.labels.\"app.kubernetes.io/name\"",
            path!["spec", "labels", "app.kubernetes.io/name"],
        ),
        ("spec.env[0]", path!["spec", "env", 0]),
    ] {
        let by_str = doc.get_path(s).unwrap();
        assert_eq!(by_str, doc.get_path(&p).unwrap(), "{s}");
        assert_eq!(by_str, doc.get_path(p.clone()).unwrap(), "{s}");
        // The `&String` route, then the `String` one.
        let owned = String::from(s);
        assert_eq!(by_str, doc.get_path(&owned).unwrap(), "{s}");
        assert_eq!(by_str, doc.get_path(owned).unwrap(), "{s}");
        assert_eq!(doc.id_of(s), Some(by_str.id()));
        let spec = doc.get("spec").unwrap();
        if let Some(rest) = s.strip_prefix("spec.") {
            assert_eq!(spec.get_path(rest), Some(by_str));
        }
    }
    assert_eq!(doc.get_path(".").unwrap(), doc.root().unwrap());
    assert_eq!(doc.get_path("spec.ports[0]").unwrap().as_u64(), Some(80));
}

#[test]
fn malformed_paths_name_nothing_and_edit_nothing() {
    let mut doc = Document::parse(SRC).unwrap();
    for bad in [
        "spec..ports",
        "spec.ports[",
        "spec.ports[x]",
        "a b",
        "\"x",
        "[0]y",
    ] {
        assert!(doc.get_path(bad).is_none(), "{bad}");
        assert!(doc.id_of(bad).is_none(), "{bad}");
        assert!(doc.get("spec").unwrap().get_path(bad).is_none(), "{bad}");
        let invalid = |e: EditError| matches!(e, EditError::InvalidPath { .. });
        assert!(invalid(doc.get_path_mut(bad).unwrap_err()), "{bad}");
        assert!(invalid(doc.set_path(bad, 1).unwrap_err()), "{bad}");
        assert!(invalid(doc.get_path_or_create(bad).unwrap_err()), "{bad}");
        assert!(invalid(doc.remove_path(bad).unwrap_err()), "{bad}");
        assert_eq!(doc.to_string(), SRC);
    }
    let e = doc.set_path("a..b", 1).unwrap_err();
    assert_eq!(
        e.to_string(),
        "invalid path at byte 2: expected a key after `.`"
    );
    assert_eq!(e.at(), None);
}

/// An empty user key must not name (or replace) the whole
/// document; `.` is the root.
#[test]
fn empty_path_is_malformed_not_the_root() {
    let mut doc = Document::parse(SRC).unwrap();
    let user_key = String::new();
    let e = doc.set_path(&user_key, "gone").unwrap_err();
    assert!(matches!(e, EditError::InvalidPath { offset: 0, .. }));
    assert_eq!(
        e.to_string(),
        "invalid path at byte 0: an empty path: write `.` for the root"
    );
    assert_eq!(doc.to_string(), SRC, "comments and content kept");
    assert!(doc.get_path("").is_none());
    assert!(doc.id_of("").is_none());
    assert!(doc.get("spec").unwrap().get_path("").is_none());
    assert!(doc.get_path_mut("").is_err());
    assert!(doc.get_path_or_create("").is_err());
    assert!(doc.remove_path("").is_err());
    assert_eq!(doc.to_string(), SRC);
    // `.` and the empty `Path` still name the root.
    let root = doc.root().unwrap();
    assert_eq!(doc.get_path(".").unwrap(), root);
    let root_path = Path::new();
    assert_eq!(doc.get_path(&root_path).unwrap(), root);
    assert_eq!(root_path.to_string(), ".");
    assert_eq!(".".parse::<Path>(), Ok(Path::new()));
}

#[test]
fn edits_take_values_and_references() {
    #[derive(Serialize)]
    struct Port {
        port: u16,
    }
    let mut doc = Document::parse(SRC).unwrap();
    doc.set_path("name", "api").unwrap();
    doc.set_path("spec.replicas", 3).unwrap();
    doc.set_path(&path!["spec", "region"], String::from("eu"))
        .unwrap();
    let mut ports = doc.get_path_mut("spec.ports").unwrap();
    ports.push(8080).unwrap();
    ports.push(8443).unwrap();
    ports.insert_at(0, 22u16).unwrap();
    ports.replace_at(1, Value::from(81)).unwrap();
    let mut env = doc.get_path_mut("spec.env").unwrap();
    env.push(String::from("DEBUG=0")).unwrap();
    env.push("A=1").unwrap();
    let mut spec = doc.get_mut("spec").unwrap();
    spec.insert("probe", Port { port: 9000 }).unwrap();
    spec.insert("tier", "gold").unwrap();
    doc.get_mut("spec")
        .unwrap()
        .entry("owner")
        .unwrap()
        .or_insert(String::from("ops"))
        .unwrap();
    doc.get_mut("name")
        .unwrap()
        .set(String::from("api2"))
        .unwrap();
    assert_eq!(
        doc.to_string(),
        "\
# app config
name: api2 # public name
spec:
  ports: [22, 81, 443, 8080, 8443]
  labels:
    app.kubernetes.io/name: web
  env:
    - LOG=info
    - DEBUG=0
    - A=1
  replicas: 3
  region: eu
  probe:
    port: 9000
  tier: gold
  owner: ops
"
    );
    let removed = doc.remove_path("spec.probe").unwrap();
    assert_eq!(removed.key(), Some("probe"));
    doc.get_path_or_create("spec.extra.list[0]").unwrap();
    assert!(doc.get_path("spec.extra.list[0]").unwrap().is_null());
    doc.set_root(vec![1, 2]).unwrap();
    // The top comment (no blank line after it) is the first entry's (rule 3),
    // but it stands outside the root's text: it stays above the new root.
    assert_eq!(doc.to_string(), "# app config\n- 1\n- 2\n");
}

#[test]
fn stream_twins_take_strings_and_values() {
    let mut stream = Stream::parse("a: {b: 1}\n---\nc: [1]\n").unwrap();
    let mut second = stream.document_mut(1).unwrap();
    second.set_path("c[1]", 2).unwrap();
    second.get_path_mut("c").unwrap().push(3).unwrap();
    second.get_path_or_create("d.e").unwrap();
    let _ = second.remove_path("d").unwrap();
    assert!(matches!(
        second.set_path("c[", 1).unwrap_err(),
        EditError::InvalidPath { offset: 1, .. }
    ));
    // The empty string is not the root in a stream either.
    assert!(matches!(
        second.set_path("", 1).unwrap_err(),
        EditError::InvalidPath { offset: 0, .. }
    ));
    let mut first = stream.document_mut(0).unwrap();
    first.set_path("a.b", "x").unwrap();
    first.set_root(Value::from("root")).unwrap();
    assert_eq!(stream.to_string(), "root\n---\nc: [1, 2, 3]\n");
}

#[test]
fn path_keys_are_text() {
    // `80` is an integer key: paths (text keys) do not reach it; `get` does.
    let doc = Document::parse("ports:\n  80: http\n  '443': https\n").unwrap();
    assert!(doc.get_path("ports.80").is_none());
    assert_eq!(
        doc.get_path("ports.443").unwrap().as_str().as_deref(),
        Some("https")
    );
    let ports = doc.get("ports").unwrap();
    assert_eq!(ports.get(80).unwrap().as_str().as_deref(), Some("http"));
    assert_eq!(Path::parse("ports.80").unwrap(), path!["ports", "80"]);
}
