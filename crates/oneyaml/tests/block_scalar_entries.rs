//! `Document` edit API fixes:
//! block scalar entries removed and moved (as in
//! GitHub Actions `run: |` steps, ConfigMap `data`), comments carried by
//! moves, no spurious blank lines, a clean document next to an
//! erroring one, and renames undone as one edit group.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, Limits, Path, Settings, Stream, Value, path};
use oneyaml_syntax::tree::{SyntaxKind, SyntaxTree};

const NLS: [&str; 3] = ["\n", "\r\n", "\r"];

fn comments(d: &Document) -> usize {
    d.syntax()
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Comment)
        .count()
}

/// Lines with nothing on them.
fn blank_lines(text: &str) -> usize {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    t.split('\n').filter(|l| l.is_empty()).count()
}

/// The edited tree equals the tree of its own text, and the
/// document reads as its text.
fn reparses(d: &Document, ctx: &str) {
    let text = d.to_string();
    let again = SyntaxTree::parse_with(text.as_str(), d.settings());
    assert!(
        *d.syntax() == again,
        "tree differs from its re-parse: {ctx}\n{text:?}"
    );
    agrees_with_its_text(d);
}

fn at(p: &Path, seg: &str) -> Path {
    match seg.parse::<usize>() {
        Ok(i) => p.clone().with_index(i),
        Err(_) => p.clone().with_key(seg),
    }
}

/// The entries of the collection at `p`, in order: (key, value), the key
/// `Null` for items.
fn entries(d: &Document, p: &Path) -> Vec<(Value, Value)> {
    let v = Value::try_from(d).unwrap();
    let mut c = &v;
    for s in p.segments() {
        if let oneyaml::PathSegment::Key(k) = s {
            c = &c[k.as_str()];
        }
    }
    match c {
        Value::Mapping(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Value::Sequence(s) => s.iter().map(|v| (Value::Null, v.clone())).collect(),
        other => panic!("not a collection: {other:?}"),
    }
}

/// The known triggers, over LF, CRLF and CR.
/// Comment rules 6 and 7: comment
/// lines after the *last* entry are the footer and stay at the end.
/// Comment rule 4: comment lines after a
/// *middle* entry's block scalar lead the next entry and move with it.
#[test]
fn block_scalar_removals_and_moves_work_and_carry_comments() {
    let removes: &[(&str, &[&str], &str)] = &[
        ("k: 1\nm: |\n  t2\n", &["m"], "k: 1\n"),
        ("- a\n- |\n  b\n", &["1"], "- a\n"),
        (
            "s:\n  - a\n  - |\n    b\nz: 1\n",
            &["s", "1"],
            "s:\n  - a\nz: 1\n",
        ),
        (
            "steps:\n  - name: x\n    run: |\n      echo hi\n  - name: y\n",
            &["steps", "0", "run"],
            "steps:\n  - name: x\n  - name: y\n",
        ),
        (
            "data:\n  a: 1\n  s.sh: |\n    echo\n",
            &["data", "s.sh"],
            "data:\n  a: 1\n",
        ),
        // No spurious blank line. Comment rule 4:
        // the comment line after the block scalar of
        // a middle entry leads the next entry and stays with it (0.2 took
        // it along provisionally).
        (
            "k: 1\nm: |\n  t2\n# c2\nz: 3\n",
            &["m"],
            "k: 1\n# c2\nz: 3\n",
        ),
        // Comment rule 7: after the last entry, the document
        // footer, which stays.
        ("- |\n  a\n- |\n  b\n# c2\n", &["1"], "- |\n  a\n# c2\n"),
    ];
    for nl in NLS {
        for (src, p, want) in removes {
            let (src, want) = (src.replace('\n', nl), want.replace('\n', nl));
            let mut d = Document::parse(src.as_str()).unwrap();
            let before = comments(&d);
            let path = p.iter().fold(Path::new(), |acc, s| at(&acc, s));
            let r = d
                .remove_path(&path)
                .unwrap_or_else(|e| panic!("{src:?}: {e:?}"));
            assert_eq!(d.to_string(), want, "{src:?}");
            assert_eq!(comments(&d) + r.comments().len(), before, "{src:?}");
            reparses(&d, &src);
        }
    }
    let moves: &[(&str, &[&str], usize, usize, &str)] = &[
        (
            "k: |\n  text\n# c\nm: |\n  t2\n# c2\n",
            &[],
            1,
            0,
            // Comment rule 4: `# c` leads `m` and moves with it (to the
            // top: rule 3); `# c2` is the document footer (rule 7).
            "# c\nm: |\n  t2\nk: |\n  text\n# c2\n",
        ),
        (
            "o:\n  k: |\n    s\n  m: |\n    t\n  # c2\nz: 1\n",
            &["o"],
            1,
            0,
            "o:\n  m: |\n    t\n  k: |\n    s\n  # c2\nz: 1\n",
        ),
        (
            "- |\n  a\n- |\n  b\n# c2\n",
            &[],
            1,
            0,
            "- |\n  b\n- |\n  a\n# c2\n",
        ),
        (
            "- |\n  a\n# c1\n- |\n  b\n# c2\n",
            &[],
            1,
            0,
            "# c1\n- |\n  b\n- |\n  a\n# c2\n",
        ),
        (
            "k: |\n  text\n# c\nm: 2\n",
            &[],
            0,
            1,
            "# c\nm: 2\nk: |\n  text\n",
        ),
        (
            "k: |\n  text\n# c\nm: 2\n",
            &[],
            1,
            0,
            "# c\nm: 2\nk: |\n  text\n",
        ),
        (
            "- |\n  text\n# c\n- 2\n",
            &[],
            0,
            1,
            "# c\n- 2\n- |\n  text\n",
        ),
        (
            "- |\n  text\n# c\n- 2\n",
            &[],
            1,
            0,
            "# c\n- 2\n- |\n  text\n",
        ),
        ("a: 1\nk: |\n  text\n", &[], 1, 0, "k: |\n  text\na: 1\n"),
    ];
    for nl in NLS {
        for (src, p, from, to, want) in moves {
            let (src, want) = (src.replace('\n', nl), want.replace('\n', nl));
            let mut d = Document::parse(src.as_str()).unwrap();
            let before = comments(&d);
            let path = p.iter().fold(Path::new(), |acc, s| at(&acc, s));
            d.get_path_mut(&path)
                .unwrap()
                .move_entry(*from, *to)
                .unwrap_or_else(|e| panic!("{src:?} {from}->{to}: {e:?}"));
            assert_eq!(d.to_string(), want, "{src:?} {from}->{to}");
            assert_eq!(comments(&d), before, "{src:?}");
            reparses(&d, &src);
            // And `insert_removed` of the same entry, by hand.
            let mut d = Document::parse(src.as_str()).unwrap();
            let entry = at(&path, &from.to_string());
            let entry = match d.get_path(&path).unwrap().as_mapping() {
                Some(m) => {
                    let k = m.iter().nth(*from).unwrap().0.as_scalar().unwrap();
                    path.clone().with_key(k.value().into_owned())
                }
                None => entry,
            };
            let r = d.remove_path(&entry).unwrap();
            let mut coll = d.get_path_mut(&path).unwrap();
            coll.insert_removed_at(*to, r).unwrap();
            assert_eq!(comments(&d), before, "{src:?} insert_removed");
            reparses(&d, &src);
        }
    }
}

/// The systematic sweep. For every block scalar style, place
/// of the block entry, collection shape, trailing comment line or none,
/// and line break: every `remove_path` and every `move_entry` pair is
/// accepted and equals its re-parse, with the meaning expected and the
/// comments conserved (no blank line appears), or it is refused as
/// `Unsupported` with a reason. `NotWellFormed` is a failure: none is
/// justified in this sweep.
#[test]
fn block_scalars_everywhere_remove_and_move() {
    let styles = ["|", "|-", "|+", ">", ">-", ">+"];
    let (mut accepted, mut refused, mut failed) = (0usize, Vec::new(), Vec::new());
    for style in styles {
        for pos in 0..3 {
            for shape in ["map", "seq", "nested map", "nested seq"] {
                for comment in [false, true] {
                    for nl in NLS {
                        let src = sweep_doc(style, pos, shape, comment).replace('\n', nl);
                        let coll = if shape.starts_with("nested") {
                            path!["o"]
                        } else {
                            Path::new()
                        };
                        let base = Document::parse(src.as_str())
                            .unwrap_or_else(|e| panic!("{src:?}: {e}"));
                        let orig = entries(&base, &coll);
                        assert_eq!(orig.len(), 3, "{src:?}");
                        let map = shape.ends_with("map");
                        let mut ops: Vec<(usize, Option<usize>)> =
                            (0..3).map(|i| (i, None)).collect();
                        for f in 0..3 {
                            for t in 0..3 {
                                if f != t {
                                    ops.push((f, Some(t)));
                                }
                            }
                        }
                        for (from, to) in ops {
                            let mut d = base.clone();
                            let ctx = format!("{src:?} {from}->{to:?}");
                            let (r, taken) = match to {
                                None => {
                                    let p = if map {
                                        coll.clone().with_key(format!("k{from}"))
                                    } else {
                                        coll.clone().with_index(from)
                                    };
                                    match d.remove_path(&p) {
                                        Ok(r) => (Ok(()), r.comments().len()),
                                        Err(e) => (Err(e), 0),
                                    }
                                }
                                Some(t) => {
                                    let r = d.get_path_mut(&coll).unwrap().move_entry(from, t);
                                    (r, 0)
                                }
                            };
                            match r {
                                Ok(()) => {
                                    accepted += 1;
                                    reparses(&d, &ctx);
                                    assert_eq!(comments(&d) + taken, comments(&base), "{ctx}");
                                    assert!(
                                        blank_lines(&d.to_string()) <= blank_lines(&src),
                                        "a blank line appeared: {ctx}\n{:?}",
                                        d.to_string()
                                    );
                                    let mut want = orig.clone();
                                    let e = want.remove(from);
                                    if let Some(t) = to {
                                        want.insert(t, e);
                                    }
                                    assert_eq!(entries(&d, &coll), want, "{ctx}");
                                }
                                Err(EditError::Unsupported { reason, .. }) => {
                                    assert!(!reason.is_empty());
                                    refused.push(format!("{ctx}: {reason}"));
                                }
                                Err(e) => failed.push(format!("refused as {e:?}: {ctx}")),
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "block scalar sweep: {accepted} accepted, {} refused as Unsupported",
        refused.len()
    );
    for r in &refused {
        eprintln!("  {r}");
    }
    for f in &failed {
        eprintln!("  FAILED {f}");
    }
    assert!(failed.is_empty(), "{} failures", failed.len());
    assert_eq!(accepted + refused.len(), 6 * 3 * 4 * 2 * 3 * 9);
    assert!(refused.is_empty(), "{refused:?}");
}

/// A document of the sweep: three entries (`k0`–`k2`, or items), the one
/// at `pos` a block scalar of `style` (keep styles with a kept blank
/// line), then a comment line when `comment`; nested ones under `o:`,
/// followed by `z: 1`.
fn sweep_doc(style: &str, pos: usize, shape: &str, comment: bool) -> String {
    let nested = shape.starts_with("nested");
    let map = shape.ends_with("map");
    let pad = if nested { "  " } else { "" };
    let mut out = String::new();
    if nested {
        out.push_str("o:\n");
    }
    for i in 0..3 {
        let head = if map {
            format!("{pad}k{i}:")
        } else {
            format!("{pad}-")
        };
        if i == pos {
            out.push_str(&format!(
                "{head} {style}\n{pad}  line one\n{pad}  line two\n"
            ));
            if style.ends_with('+') {
                out.push('\n');
            }
            if comment {
                out.push_str(&format!("{pad}# c{i}\n"));
            }
        } else {
            out.push_str(&format!("{head} v{i}\n"));
        }
    }
    if nested {
        out.push_str("z: 1\n");
    }
    out
}

/// A clean document that only reports its neighbour's
/// error (an unclosed `[` cut off by the `---`) is editable; the document
/// holding the error node keeps its protection.
#[test]
fn a_clean_document_is_not_blamed_for_its_neighbours_error() {
    for nl in NLS {
        let src = "a: [1, 2\n---\nt: 1\nu: 2\n".replace('\n', nl);
        let mut s = Stream::parse_recovering(src.as_str()).into_value();
        let before = s.diagnostics();
        {
            let mut d = s.document_mut(1).unwrap();
            d.set_path(&path!["u"], 3).unwrap();
            d.set_path(&path!["v"], 4).unwrap();
            let _ = d.remove_path(&path!["t"]).unwrap();
        }
        assert_eq!(
            s.to_string(),
            format!("a: [1, 2{nl}---{nl}u: 3{nl}v: 4{nl}")
        );
        assert_eq!(s.diagnostics(), before);
        let re = Stream::parse_recovering(s.to_string()).into_value();
        assert_eq!(re.diagnostics(), s.diagnostics());
        // The error node's document stays protected.
        let mut d0 = s.document_mut(0).unwrap();
        assert!(matches!(
            d0.set_path(&path!["a"], 1),
            Err(EditError::ReadOnly { .. })
        ));
        assert!(matches!(
            d0.remove_path(&path!["a"]),
            Err(EditError::ReadOnly { .. })
        ));
    }
}

/// A rename refused part-way (the alias step exceeds
/// `max_document_size`) leaves the text and every syntax handle as they
/// were (it used to write the anchor back by hand: a new token handle).
#[test]
fn a_refused_rename_changes_no_handle() {
    let src = "a: &x 1\nb: *x\n";
    let long = "y".repeat(20);
    let limits = Limits::default().max_document_size(src.len() + 19);
    let mut d = Document::parse_with(src, &Settings::new().limits(limits)).unwrap();
    let ids: Vec<_> = d.syntax().root().descendants().map(|n| n.id()).collect();
    let one = d.get_path(&path!["a"]).unwrap().id();
    let r = d.set_anchor(one, Some(&long));
    assert!(matches!(r, Err(EditError::Rejected { .. })), "{r:?}");
    assert_eq!(d.to_string(), src);
    for id in &ids {
        assert!(d.syntax().get(*id).is_ok(), "handle {id:?} went stale");
    }
    // A rename that fits is accepted and keeps the node's handle.
    let mut d = Document::parse(src).unwrap();
    let one = d.get_path(&path!["a"]).unwrap().id();
    assert_eq!(d.set_anchor(one, Some(&long)).unwrap(), one);
    assert_eq!(d.to_string(), format!("a: &{long} 1\nb: *{long}\n"));
    reparses(&d, src);
}
