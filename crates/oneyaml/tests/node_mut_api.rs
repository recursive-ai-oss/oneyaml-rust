//! The ergonomic edit API — one editing style on `NodeMut`, the
//! handle rule, `Option` lookups, keys of any `AsKey` type, comment lines
//! of any iterable, and (g) setting a node to the value it already has is
//! a byte no-op that keeps its style and presentation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod work;

use std::path::{Path as FsPath, PathBuf};

use oneyaml::{
    CommentOwner, Document, EditError, Mapping, NO_COMMENTS, NodeId, NodeKind, NodeRef, Settings,
    Stream, Value, path,
};

/// The edited text means what the edited document means.
fn agrees_with_its_text(d: &Document) {
    let again = Document::parse_with(d.to_string(), d.settings()).unwrap();
    assert_eq!(again.to_value().unwrap(), d.to_value().unwrap());
}

// ============================================================ (g) no-op

/// Each shape set to the value it means: no byte changes, the handle
/// stays, the value reads back.
#[test]
fn equal_values_change_no_byte() {
    let cases: &[(&str, oneyaml::Path)] = &[
        ("k: |+\n  x\n", path!["k"]),
        ("k: |+\n  x\n\nz: 1\n", path!["k"]),
        ("k: >-\n  folded\n  text\n", path!["k"]),
        ("n: 0x10 # hex\n", path!["n"]),
        ("n: 1e3\n", path!["n"]),
        ("q: 'single'\n", path!["q"]),
        ("q: \"dq\\tx\"\n", path!["q"]),
        ("t: !!str 1\n", path!["t"]),
        ("e:\nz: 1\n", path!["e"]),
        ("e: ~\n", path!["e"]),
        ("b: True\n", path!["b"]),
        ("a: &x [1,  2] # flow\n", path!["a"]),
        (
            "m:\n  # lead\n  a: 1 # one\n\n  b: [x, {y: 2}]\n  # foot\nz: 0\n",
            path!["m"],
        ),
        ("s:\n  - 1\n  -   two\n  - - 3\n", path!["s"]),
        ("- one\n- two\n", path![1]),
        ("root: 1\n", path![]),
        ("? complex\n: v\n", path!["complex"]),
        ("--- !!set\n? a\n? b\n", path!["b"]),
        ("k: !custom {a: 1}\n", path!["k"]),
        ("k: \"\"\n", path!["k"]),
    ];
    for (src, p) in cases {
        let mut d = Document::parse(*src).unwrap();
        let node = d.get_path(p).unwrap();
        let (id, v) = (node.id(), node.deserialize::<Value>().unwrap());
        let mut n = d.node_mut(id).unwrap();
        n.set(&v).unwrap();
        assert_eq!(n.id(), id, "{src:?}");
        assert_eq!(d.to_string().as_bytes(), src.as_bytes(), "{src:?}");
        assert!(d.node(id).is_ok(), "{src:?}");
        // Through the other setters too.
        d.set_path(p, &v).unwrap();
        assert_eq!(d.to_string().as_bytes(), src.as_bytes(), "{src:?}");
        agrees_with_its_text(&d);
    }
}

/// Numbers equal by `Value` equality: another radix or form of the same
/// integer is no change.
#[test]
fn equal_means_value_equality() {
    let mut d = Document::parse("a: 0x10\nb: 0o20\nc: 16\nd: 1.50\n").unwrap();
    d.set_path(&path!["a"], 16).unwrap();
    d.set_path(&path!["b"], 16u8).unwrap();
    d.set_path(&path!["c"], 16i64).unwrap();
    d.set_path(&path!["d"], 1.5).unwrap();
    assert_eq!(d.to_string(), "a: 0x10\nb: 0o20\nc: 16\nd: 1.50\n");
    // Another value is written as before, comments kept.
    d.set_path(&path!["a"], 17).unwrap();
    assert_eq!(d.to_string(), "a: 17\nb: 0o20\nc: 16\nd: 1.50\n");
    // A string is not the number it reads like.
    d.set_path(&path!["c"], "16").unwrap();
    assert_eq!(d.to_string(), "a: 17\nb: 0o20\nc: '16'\nd: 1.50\n");
}

/// The rest of an equal-value set: `set_scalar`, `replace_at`, `insert`
/// on an existing key, `set_root`, and a value in a stream.
#[test]
fn every_setter_keeps_equal_values() {
    let src = "k: 'a' # c\nl: [x, \"y\"]\n";
    let mut d = Document::parse(src).unwrap();
    d.get_mut("k").unwrap().set_scalar("a").unwrap();
    d.get_mut("l").unwrap().replace_at(1, "y").unwrap();
    d.root_mut().unwrap().insert("k", "a").unwrap();
    let root = d.to_value().unwrap();
    d.set_root(&root).unwrap();
    assert_eq!(d.to_string(), src);
    let mut s = Stream::parse(format!("{src}---\n{src}")).unwrap();
    s.document_mut(1)
        .unwrap()
        .set_path(&path!["l"], vec!["x", "y"])
        .unwrap();
    assert_eq!(s.to_string(), format!("{src}---\n{src}"));
    // Recorded as an edit: the stream's diagnostics are read as edited
    // (equal to a parse of the text).
    assert_eq!(
        s.diagnostics(),
        Stream::parse(s.to_string()).unwrap().diagnostics()
    );
}

/// What stays refused: a key (as before), an error region, a read-only
/// document.
#[test]
fn equal_values_are_still_checked() {
    let mut d = Document::parse("a: 1\n").unwrap();
    let key = d.get("a").unwrap().key().unwrap().id();
    let e = d.node_mut(key).unwrap().set("a").unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");

    let mut d = Document::parse_recovering("a: [1\nb: 2\n").into_value();
    let err = d.errors().next().unwrap().id();
    assert!(
        d.node_mut(err)
            .and_then(|mut n| n.set(1).map(|_| ()))
            .is_err()
    );

    let mut d = Document::from_slice_recovering(b"a: 1\nb: \xff\n", &Settings::new()).into_value();
    let e = d.set_path(&path!["a"], 1).map(|_| ()).unwrap_err();
    assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");

    // Repeated keys (`LastWins`): the node is rewritten, dropping the
    // repeats, at the top level and nested alike (the same rule
    // at every depth), as for another meaning.
    let s = Settings::new().duplicate_keys(oneyaml::DuplicateKeys::LastWins);
    for (src, p, want) in [
        ("m:\n  a: 1\n  a: 1\n", path!["m"], "m:\n  a: 1\n"),
        (
            "o:\n  m:\n    a: 1\n    a: 2\n",
            path!["o"],
            "o:\n  m:\n    a: 2\n",
        ),
    ] {
        let mut d = Document::parse_with(src, &s).unwrap();
        let v = d.get_path(&p).unwrap().deserialize::<Value>().unwrap();
        d.set_path(&p, &v).unwrap();
        assert_eq!(d.to_string(), want);
    }
    let mut d = Document::parse_with("m:\n  a: 1\n  a: 2\n", &s).unwrap();
    d.set_path(&path!["m"], oneyaml::from_str::<Value>("{a: 3}").unwrap())
        .unwrap();
    assert_eq!(d.to_string(), "m:\n  a: 3\n");
}

/// Merge keys: a mapping set to the meaning it has with its merged keys
/// is written as that meaning (the merge is a link: aliases denote
/// the node); the document means what it meant.
#[test]
fn merged_meaning_is_written() {
    let settings = Settings::new().merge_keys(true);
    let src = "base: &b {x: 1}\nm:\n  <<: *b\n  y: 2\n";
    let mut d = Document::parse_with(src, &settings).unwrap();
    let before = d.to_value().unwrap();
    let v = d.get("m").unwrap().deserialize::<Value>().unwrap();
    assert_eq!(v["x"].as_u64(), Some(1));
    d.set_path(&path!["m"], &v).unwrap();
    assert!(!d.to_string().contains("<<"), "{:?}", d.to_string());
    assert_eq!(d.to_value().unwrap(), before);
}

/// Every value node of every valid yaml-test-suite document set to its
/// own value, under three settings (default, `merge_keys`, `LastWins`):
/// a node without aliases, merge keys and repeated keys changes no byte
/// ("in general", not only for the shapes above); a node that is or
/// holds one is written as any value, and the document still
/// means what it meant (or the edit is refused and nothing changes).
#[test]
fn suite_values_set_to_themselves_change_nothing() {
    let root = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    if !root.is_dir() {
        eprintln!("skipped: no yaml-test-suite data at {}", root.display());
        return;
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            }
        }
        if dir.join("in.yaml").is_file() && !dir.join("error").exists() {
            dirs.push(dir);
        }
    }
    dirs.sort();
    let passes = [
        ("default", Settings::new()),
        ("merge_keys", Settings::new().merge_keys(true)),
        (
            "LastWins",
            Settings::new().duplicate_keys(oneyaml::DuplicateKeys::LastWins),
        ),
    ];
    for (label, settings) in &passes {
        let (mut kept, mut linked, mut docs) = (0usize, 0usize, 0usize);
        for dir in &dirs {
            let src = std::fs::read_to_string(dir.join("in.yaml")).unwrap();
            let Ok(d) = Document::parse_with(src.as_str(), settings) else {
                continue; // several documents: a `Stream`
            };
            let Ok(meaning) = d.to_value() else { continue };
            docs += 1;
            let merges = settings.get_merge_keys();
            for (id, v) in value_nodes(&d) {
                let link = has_link(d.node(id).unwrap(), merges);
                let mut e = d.clone();
                let r = e.node_mut(id).unwrap().set(&v).map(|n| n.id());
                if link {
                    match r {
                        Ok(_) => assert_eq!(
                            e.to_value().unwrap(),
                            meaning,
                            "{label} {} node {id:?}: {:?}",
                            dir.display(),
                            e.to_string()
                        ),
                        Err(_) => assert_eq!(e.to_string(), src),
                    }
                    linked += 1;
                } else {
                    assert!(r.is_ok(), "{label} {} node {id:?}: {r:?}", dir.display());
                    assert_eq!(e.to_string(), src, "{label} {} node {id:?}", dir.display());
                    kept += 1;
                }
            }
        }
        eprintln!("{label}: {kept} nodes kept, {linked} with links, {docs} documents");
        // (Measured: 991/37 default; floors near the counts, so a
        // corpus or traversal change that loses nodes fails here.)
        assert!(kept >= 950, "{label}: {kept}");
        assert!(linked >= 35, "{label}: {linked}");
    }
}

/// Whether `n` is or holds an alias, (with `merge_keys`) a `<<` key, or
/// a mapping with repeated keys (`LastWins`): what the no-op leaves to a
/// rewrite.
fn has_link(n: NodeRef<'_>, merges: bool) -> bool {
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if n.kind() == NodeKind::Alias {
            return true;
        }
        if let Some(m) = n.as_mapping() {
            let meant = n.deserialize::<Value>().ok();
            let meant = meant
                .as_ref()
                .and_then(|v| v.untag_ref().as_mapping())
                .map(|m| m.len());
            if meant != Some(m.len()) {
                return true;
            }
            for (k, v) in m.iter() {
                let merge = k.as_scalar().is_some_and(|s| s.value() == "<<");
                if merges && merge {
                    return true;
                }
                stack.push(k);
                stack.push(v);
            }
        } else if let Some(s) = n.as_sequence() {
            stack.extend(s.iter());
        }
    }
    false
}

/// The value nodes (not keys) of `d` and the values they mean.
fn value_nodes(d: &Document) -> Vec<(NodeId, Value)> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeRef<'_>> = d.root().into_iter().collect();
    while let Some(n) = stack.pop() {
        if let Ok(v) = n.deserialize::<Value>() {
            out.push((n.id(), v));
        }
        if n.kind() == NodeKind::Alias {
            continue;
        }
        if let Some(m) = n.as_mapping() {
            stack.extend(m.iter().map(|(_, v)| v));
        } else if let Some(s) = n.as_sequence() {
            stack.extend(s.iter());
        }
    }
    out
}

// =================================================== (g) only on setters

/// Removing the only entry of a mapping whose
/// meaning is `{}` (a merge of nothing) collapses it to `{}`: the internal
/// collapse step never takes the no-op path.
#[test]
fn collapsing_a_merge_only_mapping_writes_it() {
    let s = Settings::new().merge_keys(true);
    let cases = [
        ("e: &e {}\nm:\n  <<: *e\n", "e: &e {}\nm: {}\n"),
        ("m:\n  <<: []\n", "m: {}\n"),
        ("e: &e {}\nm:\n  <<: [*e]\n", "e: &e {}\nm: {}\n"),
        (
            "e: &e {}\nm: &n\n  <<: *e\nz: *n\n",
            "e: &e {}\nm: &n {}\nz: *n\n",
        ),
    ];
    for (src, want) in cases {
        let mut d = Document::parse_with(src, &s).unwrap();
        let v = {
            let m = d.get("m").unwrap();
            m.as_mapping().unwrap().iter().next().unwrap().1.id()
        };
        let r = d.remove_node(v).unwrap();
        assert!(r.text().starts_with("<<:"), "{src:?}");
        assert_eq!(d.to_string(), want, "{src:?}");
        agrees_with_its_text(&d);
    }
}

/// A node that is or holds an alias or a merge key, set to the
/// value it means, is written as that value: the link goes, and a later
/// edit of the anchored node no longer shows there.
#[test]
fn links_are_written_as_values() {
    let mut d = Document::parse("a: &x {k: v}\nb: *x\n").unwrap();
    d.set_path(&path!["b"], oneyaml::from_str::<Value>("{k: v}").unwrap())
        .unwrap();
    assert!(!d.to_string().contains("*x"), "{:?}", d.to_string());
    d.set_path(&path!["a", "k"], "w").unwrap();
    assert_eq!(d.get_path(&path!["b", "k"]).unwrap().text(), "v");
    agrees_with_its_text(&d);

    let mut d = Document::parse("a: &x 1\nc: [*x]\n").unwrap();
    d.set_path(&path!["c"], [1]).unwrap();
    assert_eq!(d.to_string(), "a: &x 1\nc:\n  - 1\n");
    // A handle to an alias: `set` replaces the alias itself.
    let mut d = Document::parse("a: &x 1\nb: *x\n").unwrap();
    d.get_mut("b").unwrap().set(1).unwrap();
    assert_eq!(d.to_string(), "a: &x 1\nb: 1\n");

    let s = Settings::new().merge_keys(true);
    let src = "base: &b {x: 1}\nm:\n  <<: *b\n  y: 2\n";
    let mut d = Document::parse_with(src, &s).unwrap();
    let v = d.get("m").unwrap().deserialize::<Value>().unwrap();
    d.set_path(&path!["m"], &v).unwrap();
    assert!(!d.to_string().contains("<<"), "{:?}", d.to_string());
    d.set_path(&path!["base", "x"], 5).unwrap();
    assert_eq!(d.get_path(&path!["m", "x"]).unwrap().text(), "1");
    agrees_with_its_text(&d);
}

/// The equality check costs O(the node), never what its
/// aliases reach: setting a node of a "billion laughs" shape costs the
/// same whatever the expansion (100× more here).
#[test]
fn equality_check_does_not_expand_aliases() {
    let laughs = |depth: usize| {
        let mut src = String::from("l0: &l0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..=depth {
            let p = format!("*l{}", i - 1);
            let items = vec![p; 10].join(", ");
            src.push_str(&format!("l{i}: &l{i} [{items}]\n"));
        }
        let p = format!("*l{depth}");
        src.push_str(&format!("f: [{}]\n", vec![p; 9].join(", ")));
        let s = Settings::new().limits(oneyaml::Limits::unlimited());
        Document::parse_with(src, &s).unwrap()
    };
    let per = |depth: usize| {
        let d0 = laughs(depth);
        let same_len: Vec<u8> = (1..=9).collect();
        let mut best = u64::MAX;
        for _ in 0..3 {
            let mut d = d0.clone();
            work::reset();
            d.set_path(&path!["f"], &same_len).unwrap();
            best = best.min(work::taken());
            let f = d.get("f").unwrap().deserialize::<Value>().unwrap();
            assert_eq!(f, oneyaml::to_value(&same_len).unwrap());
        }
        best
    };
    let (a, b) = (per(2), per(4));
    let r = work::ratio("set f", a, b);
    eprintln!("set f: {a:?} steps at 10^3 expanded nodes, {b:?} at 10^5 (ratio {r:.2})");
    assert!(r < 3.0, "the check scales with the expansion: {r:.2}");
}

// ====================================================== the handle rule

#[test]
fn edits_take_mut_self_and_follow_the_node() {
    let mut d = Document::parse("a: 1\nl: [x]\n").unwrap();
    let mut a = d.get_mut("a").unwrap();
    let before = a.id();
    a.set(2).unwrap().set_anchor(Some("n")).unwrap();
    assert_ne!(a.id(), before);
    assert_eq!(a.get().unwrap().anchor(), Some("n"));
    a.set_scalar("three").unwrap();
    assert_eq!(a.get().unwrap().text(), "&n three");
    let id = a.id();
    assert_eq!(d.node(id).unwrap().text(), "&n three");
    // A creator returns a handle to what it created; the parent handle
    // stays usable afterwards.
    let mut l = d.get_mut("l").unwrap();
    let y = l.push("w").unwrap().id();
    let z = l.insert_at(0, "z").unwrap().id();
    assert_eq!(l.get().unwrap().as_sequence().unwrap().len(), 3);
    assert_eq!(d.node(y).unwrap().text(), "w");
    assert_eq!(d.node(z).unwrap().text(), "z");
    assert_eq!(d.to_string(), "a: &n three\nl: [z, x, w]\n");
}

#[test]
fn into_key_names_the_entry() {
    let mut d = Document::parse("a: 1\nl: [x]\n").unwrap();
    d.get_mut("a")
        .unwrap()
        .into_key()
        .unwrap()
        .set_trailing(Some("one"))
        .unwrap();
    assert_eq!(d.to_string(), "a: 1 # one\nl: [x]\n");
    // On the value's handle, the same entry.
    d.get_mut("a").unwrap().set_trailing(Some("uno")).unwrap();
    assert_eq!(d.to_string(), "a: 1 # uno\nl: [x]\n");
    for p in [path![], path!["l", 0]] {
        let e = d.get_path_mut(&p).unwrap().into_key().unwrap_err();
        assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    }
    let key = d.get_mut("a").unwrap().into_key().unwrap().id();
    assert_eq!(d.comments(key).unwrap().trailing()[0].text(), "uno");
    // The base layer acts on exactly the id it gets: a value on its key's
    // line has no line of its own.
    let value = d.id_of(&path!["a"]).unwrap();
    let e = d.set_trailing(value, Some("x")).unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
}

/// On a mapping-value handle every comment edit acts
/// on the entry, never a silent `Ok` that does nothing or the wrong thing.
#[test]
fn value_handles_edit_their_entry() {
    let edit = |src: &str, op: u8| {
        let mut d = Document::parse(src).unwrap();
        let mut n = d.get_mut("a").unwrap();
        let r = match op {
            0 => n.set_trailing(Some("c")).map(|_| String::new()),
            1 => n.set_leading(["c"]).map(|_| String::new()),
            2 => n.clear_comments().map(|x| x.join(",")),
            _ => n.set_blank_lines_before(1).map(|()| String::new()),
        };
        (r, d.to_string())
    };
    let cases: &[(&str, u8, &str, &str)] = &[
        ("a:\n  b: 1\n", 0, "", "a: # c\n  b: 1\n"),
        ("a:\n  b: 1\n", 1, "", "# c\na:\n  b: 1\n"),
        ("a: [1]\n", 0, "", "a: [1] # c\n"),
        ("a: [1] # t\n", 0, "", "a: [1] # c\n"),
        ("x: 0\na: 1\n", 1, "", "x: 0\n# c\na: 1\n"),
        ("a: 1 # t\n", 2, "# t", "a: 1\n"),
        ("a: {b: 1} # t\n", 2, "# t", "a: {b: 1}\n"),
        ("# lead\na:\n  b: 1\n", 2, "# lead", "a:\n  b: 1\n"),
        ("a: # t\n  b: 1\n", 2, "# t", "a:\n  b: 1\n"),
        ("x: 0\na:\n  - x\n", 3, "", "x: 0\n\na:\n  - x\n"),
        ("x: 0\na: 1\n", 3, "", "x: 0\n\na: 1\n"),
        // A comment the entry does not own: nothing to clear (it is
        // `b`'s), and nothing changes.
        ("a:\n  b: 1 # t\n", 2, "", "a:\n  b: 1 # t\n"),
    ];
    for (src, op, got, text) in cases {
        let (r, after) = edit(src, *op);
        assert_eq!(r.as_deref(), Ok(*got), "{src:?} op {op}");
        assert_eq!(after, *text, "{src:?} op {op}");
        let again = Document::parse(after.as_str()).unwrap();
        assert_eq!(again.to_string(), after);
    }
}

/// The node edits of a `NodeMut` are the id-taking `Document` methods on
/// its entry (a mapping value's key) or, for `push_footer` and
/// `set_anchor`, its node: same text, same comments, same errors.
#[test]
fn node_edits_equal_the_base_layer() {
    let src = "# h\n\na: 1 # t\nl:\n  - x\nm: {k: v}\n";
    let base = Document::parse(src).unwrap();
    let key = |p: &str| base.get(p).unwrap().key().unwrap().id();
    let value = |p: &str| base.id_of(&path![p]).unwrap();
    type Edit = fn(&mut Document, NodeId, NodeId, bool) -> Result<String, EditError>;
    let edits: &[Edit] = &[
        |d, v, k, h| {
            if h {
                d.node_mut(v)?
                    .set_leading(["x", "y"])
                    .map(|c| c.len().to_string())
            } else {
                d.set_leading(k, ["x", "y"]).map(|c| c.len().to_string())
            }
        },
        |d, v, k, h| {
            if h {
                d.node_mut(v)?.set_trailing(None).map(|c| format!("{c:?}"))
            } else {
                d.set_trailing(k, None).map(|c| format!("{c:?}"))
            }
        },
        |d, v, _, h| {
            if h {
                d.node_mut(v)?.push_footer("f").map(|_| String::new())
            } else {
                d.push_footer(v, "f").map(|_| String::new())
            }
        },
        |d, v, k, h| {
            if h {
                d.node_mut(v)?.clear_comments().map(|c| c.join(","))
            } else {
                let mut c = d.clear_comments(k)?;
                c.extend(d.clear_comments(v)?);
                Ok(c.join(","))
            }
        },
        |d, v, k, h| {
            if h {
                d.node_mut(v)?
                    .set_blank_lines_before(1)
                    .map(|()| String::new())
            } else {
                d.set_blank_lines_before(k, 1).map(|()| String::new())
            }
        },
        |d, v, _, h| {
            let n = if h {
                d.node_mut(v)?.set_anchor(Some("q"))?.id()
            } else {
                d.set_anchor(v, Some("q"))?
            };
            Ok(d.node(n)?.text().into_owned())
        },
    ];
    for (i, edit) in edits.iter().enumerate() {
        for p in ["a", "l", "m"] {
            let (mut x, mut y) = (base.clone(), base.clone());
            let rx = edit(&mut x, value(p), key(p), true);
            let ry = edit(&mut y, value(p), key(p), false);
            assert_eq!(rx, ry, "edit {i} on {p}");
            assert_eq!(x.to_string(), y.to_string(), "edit {i} on {p}");
            let ex = x.explain_comments().unwrap();
            let ey = y.explain_comments().unwrap();
            let owners = |e: &[oneyaml::CommentExplanation]| -> Vec<(String, CommentOwner)> {
                e.iter().map(|c| (c.text().to_owned(), c.owner())).collect()
            };
            assert_eq!(owners(&ex), owners(&ey), "edit {i} on {p}");
        }
    }
}

// ============================================================== lookups

#[test]
fn read_lookups_are_options_and_edits_say_why() {
    let mut d = Document::parse("a: [1, {b: 2}]\n").unwrap();
    assert!(d.get_path(&path!["a", 1, "b"]).is_some());
    assert!(d.get_path(&path!["a", 5]).is_none());
    assert!(d.get_path(&path!["zz"]).is_none());
    assert!(d.id_of(&path!["zz"]).is_none());
    assert!(d.root().unwrap().get_path(&path!["a", 0, "x"]).is_none());
    // The edit API keeps the reason.
    let e = d.get_path_mut(&path!["a", 5]).unwrap_err();
    assert!(
        matches!(
            e,
            EditError::IndexOutOfBounds {
                index: 5,
                len: 2,
                ..
            }
        ),
        "{e:?}"
    );
    let e = d.get_mut("zz").unwrap_err();
    assert!(matches!(e, EditError::NotFound { .. }), "{e:?}");
    let empty = Document::parse("").unwrap();
    assert!(empty.get_path(path![]).is_none());
}

// ================================================================= keys

#[test]
fn keys_of_any_type() {
    let src = "1: one\n'1': quoted\ntrue: yes\n~: nothing\n0x10: hex\nname: n\n";
    let mut d = Document::parse(src).unwrap();
    assert_eq!(d.get(1).unwrap().text(), "one");
    assert_eq!(d.get(1u8).unwrap().text(), "one");
    assert_eq!(d.get(1i64).unwrap().text(), "one");
    assert_eq!(d.get("1").unwrap().text(), "quoted");
    assert_eq!(d.get(String::from("1")).unwrap().text(), "quoted");
    let name = String::from("name");
    assert_eq!(d.get(&name).unwrap().text(), "n");
    assert_eq!(d.get(name).unwrap().text(), "n");
    assert_eq!(d.get(true).unwrap().text(), "yes");
    assert_eq!(d.get(Value::Null).unwrap().text(), "nothing");
    let sixteen = Value::from(16);
    assert_eq!(d.get(&sixteen).unwrap().text(), "hex");
    assert_eq!(d.get(sixteen).unwrap().text(), "hex");
    assert_eq!(d.get(16usize).unwrap().text(), "hex");
    assert!(d.get(2).is_none() && d.get(false).is_none());
    assert_eq!(d.root().unwrap().get(16).unwrap().text(), "hex");

    d.get_mut(1).unwrap().set("uno").unwrap();
    d.get_mut(true).unwrap().set_scalar("si").unwrap();
    let mut root = d.root_mut().unwrap();
    root.insert(2, "two").unwrap();
    root.insert(false, 0).unwrap();
    root.insert(16, "sixteen").unwrap();
    root.insert(Value::Null, "nil").unwrap();
    assert!(
        !root
            .get()
            .unwrap()
            .as_mapping()
            .unwrap()
            .iter()
            .any(|(k, _)| k.text() == "2.0")
    );
    let gone = root.remove(2).unwrap();
    assert_eq!(gone.text(), "2: two");
    let e = root.remove(3).unwrap_err();
    assert_eq!(e.at(), Some(&path!["3"]));
    assert_eq!(
        d.to_string(),
        "1: uno\n'1': quoted\ntrue: si\n~: nil\n0x10: sixteen\nname: n\nfalse: 0\n"
    );
    agrees_with_its_text(&d);

    // `insert_alias` with a key of any type.
    let mut d = Document::parse("a: &x 1\nm:\n  k: 0\n").unwrap();
    let x = d.id_of(&path!["a"]).unwrap();
    let mut m = d.get_mut("m").unwrap();
    m.insert_alias(1, x).unwrap();
    m.insert_alias(String::from("s"), x).unwrap();
    let e = m.insert_alias(1u64, x).map(|_| ()).unwrap_err();
    assert!(matches!(e, EditError::KeyExists { .. }), "{e:?}");
    assert_eq!(d.to_string(), "a: &x 1\nm:\n  k: 0\n  1: *x\n  s: *x\n");
    agrees_with_its_text(&d);

    // `entry` with a key of any type.
    let mut d = Document::parse("1: a\n").unwrap();
    let e = d.root_mut().unwrap().entry(1).unwrap();
    assert!(e.is_occupied());
    let e = d.root_mut().unwrap().entry(true).unwrap();
    assert!(!e.is_occupied());
    e.or_insert("b").unwrap();
    assert_eq!(d.to_string(), "1: a\ntrue: b\n");
}

#[test]
fn collection_keys_are_not_inserted() {
    let mut d = Document::parse("a: 1\n").unwrap();
    let mut m = Mapping::new();
    m.insert(Value::from("x"), Value::from(1));
    let e = d
        .root_mut()
        .unwrap()
        .insert(Value::Mapping(m), 1)
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    let e = d
        .root_mut()
        .unwrap()
        .insert(Value::Sequence(vec![Value::from(1)].into()), 1)
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    assert_eq!(d.to_string(), "a: 1\n");
}

/// A key written in flow context, and a tagged scalar key.
#[test]
fn non_string_keys_in_flow_and_tagged() {
    let mut d = Document::parse("m: {a: 1}\n").unwrap();
    d.get_mut("m").unwrap().insert(7, "seven").unwrap();
    assert_eq!(d.to_string(), "m: {a: 1, 7: seven}\n");
    let tagged = Value::Tagged(Box::new(oneyaml::TaggedValue {
        tag: oneyaml::Tag::new("!k"),
        value: Value::from("x"),
    }));
    d.get_mut("m").unwrap().insert(&tagged, 1).unwrap();
    assert_eq!(d.get("m").unwrap().get(&tagged).unwrap().text(), "1");
    agrees_with_its_text(&d);
}

// ======================================================== comment lines

#[test]
fn comment_lines_of_any_iterable() {
    let mut d = Document::parse("a: 1\nb: 2\n").unwrap();
    let owned: Vec<String> = vec!["one".into(), "two".into()];
    let mut b = d.get_mut("b").unwrap().into_key().unwrap();
    b.set_leading(&owned).unwrap();
    b.set_leading(owned.iter().map(String::as_str)).unwrap();
    b.set_leading(["x"]).unwrap();
    b.set_leading(&["y"]).unwrap();
    b.set_leading(vec!["z"]).unwrap();
    assert_eq!(d.to_string(), "a: 1\n# z\nb: 2\n");
    d.get_mut("b")
        .unwrap()
        .into_key()
        .unwrap()
        .set_leading(NO_COMMENTS)
        .unwrap();
    d.set_header(vec![String::from("head")]).unwrap();
    d.set_footer(["foot"]).unwrap();
    assert_eq!(d.to_string(), "# head\n\na: 1\nb: 2\n# foot\n");
    d.set_header(NO_COMMENTS).unwrap();
    d.set_footer(NO_COMMENTS).unwrap();
    // (The header's blank line stays: blank lines are not comments.)
    assert_eq!(d.to_string(), "\na: 1\nb: 2\n");
}

/// The stream guard's node edits go through its handles; the stream
/// changes.
#[test]
fn stream_node_edits_through_the_guard() {
    let mut s = Stream::parse("a: 1\n---\nb: 2\nl: [x]\n").unwrap();
    let mut doc = s.document_mut(1).unwrap();
    doc.get_mut("b")
        .unwrap()
        .into_key()
        .unwrap()
        .set_leading(["about b"])
        .unwrap();
    doc.get_mut("l")
        .unwrap()
        .push("w")
        .unwrap()
        .set_anchor(Some("w"))
        .unwrap();
    doc.set_path(&path!["b"], 3)
        .unwrap()
        .into_key()
        .unwrap()
        .set_trailing(Some("now 3"))
        .unwrap();
    assert_eq!(
        s.to_string(),
        "a: 1\n---\n# about b\nb: 3 # now 3\nl: [x, &w w]\n"
    );
}
