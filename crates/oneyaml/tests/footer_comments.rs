//! Comment lines after a block collection's **last** entry are
//! the collection's footer (comment rule 6) or, at root indentation,
//! the document footer (rule 7), whatever the style of that entry's value
//! (the document header and footer change only through
//! explicit calls). Remove leaves them after the collection, a move or a
//! re-insertion leaves them at the end, an append goes before them; a
//! plain last value and a block scalar last value behave the same.
//!
//! Also pinned: the 0.2 limitation class of atomic corner shapes (block
//! scalars next to comments and blank lines), which the
//! rulebook closes (comment rules 2-7).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, Path, path};
use oneyaml_syntax::tree::{SyntaxKind, SyntaxTree};

const NLS: [&str; 3] = ["\n", "\r\n", "\r"];

fn comments(d: &Document) -> usize {
    d.syntax()
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Comment)
        .count()
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

/// A collection shape of the parity test.
#[derive(Clone, Copy, Debug)]
struct Shape {
    map: bool,
    nested: bool,
}

const SHAPES: [Shape; 4] = [
    Shape {
        map: true,
        nested: false,
    },
    Shape {
        map: false,
        nested: false,
    },
    Shape {
        map: true,
        nested: true,
    },
    Shape {
        map: false,
        nested: true,
    },
];

/// One entry: its name (key, and value `v{name}`), and whether its value
/// is written as a `|` block scalar.
type Entry = (&'static str, bool);

/// The document of `entries` in `shape`, then the footer comment line
/// (at the collection's indentation: a collection footer under `o:`,
/// followed by `z: 1`, or the document footer at the root).
fn doc(shape: Shape, entries: &[Entry], nl: &str) -> String {
    let pad = if shape.nested { "  " } else { "" };
    let mut out = String::new();
    if shape.nested {
        out.push_str("o:\n");
    }
    for (name, block) in entries {
        let head = if shape.map {
            format!("{pad}{name}:")
        } else {
            format!("{pad}-")
        };
        if *block {
            out.push_str(&format!("{head} |\n{pad}  v{name}\n"));
        } else {
            out.push_str(&format!("{head} v{name}\n"));
        }
    }
    out.push_str(&format!("{pad}# footer\n"));
    if shape.nested {
        out.push_str("z: 1\n");
    }
    out.replace('\n', nl)
}

fn coll_path(shape: Shape) -> Path {
    if shape.nested {
        path!["o"]
    } else {
        Path::new()
    }
}

fn entry_path(shape: Shape, name: &str, index: usize) -> Path {
    if shape.map {
        coll_path(shape).with_key(name)
    } else {
        coll_path(shape).with_index(index)
    }
}

/// The required behaviour: for each shape, the plain-value and the
/// block-scalar variant of the last entry, under remove, move to the
/// front, append, and remove + re-insert at the end, over LF, CRLF and
/// CR, keep the footer in the same place (after the collection, after
/// every entry), and conserve every comment.
#[test]
fn footer_after_the_last_entry_stays_whatever_its_value_style() {
    for nl in NLS {
        for shape in SHAPES {
            for block in [false, true] {
                let orig: [Entry; 3] = [("a", false), ("b", false), ("k", block)];
                let src = doc(shape, &orig, nl);
                let ctx = |op: &str| format!("{shape:?} block={block} {op}: {src:?}");
                let base = Document::parse(src.as_str()).unwrap();
                let total = comments(&base);

                // Remove the last entry: the footer stays; the removal
                // takes no comment.
                let mut d = base.clone();
                let r = d.remove_path(entry_path(shape, "k", 2)).unwrap();
                assert_eq!(
                    d.to_string(),
                    doc(shape, &orig[..2], nl),
                    "{}",
                    ctx("remove")
                );
                assert!(
                    r.comments().is_empty(),
                    "{}: {:?}",
                    ctx("remove"),
                    r.comments()
                );
                assert_eq!(comments(&d), total, "{}", ctx("remove"));
                reparses(&d, &ctx("remove"));

                // Re-insert it at the end: back before the footer, the
                // original text.
                d.get_path_mut(coll_path(shape))
                    .unwrap()
                    .insert_removed(r)
                    .unwrap();
                assert_eq!(d.to_string(), src, "{}", ctx("re-insert"));
                reparses(&d, &ctx("re-insert"));

                // Move the last entry to the front: the footer stays last.
                let mut d = base.clone();
                d.get_path_mut(coll_path(shape))
                    .unwrap()
                    .move_entry(2, 0)
                    .unwrap();
                let moved = [orig[2], orig[0], orig[1]];
                assert_eq!(
                    d.to_string(),
                    doc(shape, &moved, nl),
                    "{}",
                    ctx("move 2->0")
                );
                assert_eq!(comments(&d), total, "{}", ctx("move 2->0"));
                reparses(&d, &ctx("move 2->0"));

                // Move the first entry to the end (after the last one):
                // before the footer.
                let mut d = base.clone();
                d.get_path_mut(coll_path(shape))
                    .unwrap()
                    .move_entry(0, 2)
                    .unwrap();
                let moved = [orig[1], orig[2], orig[0]];
                assert_eq!(
                    d.to_string(),
                    doc(shape, &moved, nl),
                    "{}",
                    ctx("move 0->2")
                );
                assert_eq!(comments(&d), total, "{}", ctx("move 0->2"));
                reparses(&d, &ctx("move 0->2"));

                // Append: the new entry goes before the footer.
                let mut d = base.clone();
                let mut c = d.get_path_mut(coll_path(shape)).unwrap();
                if shape.map {
                    c.insert("new", "vnew").unwrap();
                } else {
                    c.push("vnew").unwrap();
                }
                let appended = [orig[0], orig[1], orig[2], ("new", false)];
                assert_eq!(
                    d.to_string(),
                    doc(shape, &appended, nl),
                    "{}",
                    ctx("append")
                );
                assert_eq!(comments(&d), total, "{}", ctx("append"));
                reparses(&d, &ctx("append"));
            }
        }
    }
}

/// Comment rule 6's own example: the commented-out item is the
/// sequence's footer, after a plain item or a block scalar item alike.
#[test]
fn rule_6_example_keeps_its_collection_footer() {
    for nl in NLS {
        for item in ["- b\n", "- |\n    b\n"] {
            let src = format!("list:\n  - a\n  {item}  # - c\nother: 1\n").replace('\n', nl);
            let n = |s: &str| s.replace('\n', nl);
            // Remove the last item.
            let mut d = Document::parse(src.as_str()).unwrap();
            let r = d.remove_path(&path!["list", 1]).unwrap();
            assert!(r.comments().is_empty(), "{src:?}");
            assert_eq!(d.to_string(), n("list:\n  - a\n  # - c\nother: 1\n"));
            reparses(&d, &src);
            // Move it to the front.
            let mut d = Document::parse(src.as_str()).unwrap();
            d.get_mut("list").unwrap().move_entry(1, 0).unwrap();
            assert_eq!(
                d.to_string(),
                n(&format!("list:\n  {item}  - a\n  # - c\nother: 1\n"))
            );
            reparses(&d, &src);
            // Append.
            let mut d = Document::parse(src.as_str()).unwrap();
            d.get_mut("list").unwrap().push("d").unwrap();
            assert_eq!(
                d.to_string(),
                n(&format!("list:\n  - a\n  {item}  - d\n  # - c\nother: 1\n"))
            );
            reparses(&d, &src);
        }
    }
}

/// The unit's own triggers, byte for byte, next to their plain twins.
#[test]
fn the_reported_triggers_match_their_plain_twins() {
    for nl in NLS {
        let n = |s: &str| s.replace('\n', nl);
        for last in ["k: 2\n", "k: |\n  text\n"] {
            let src = n(&format!("a: 1\n{last}# footer\n"));
            let mut d = Document::parse(src.as_str()).unwrap();
            let r = d.remove_path(&path!["k"]).unwrap();
            assert!(r.comments().is_empty());
            assert_eq!(d.to_string(), n("a: 1\n# footer\n"), "{src:?}");

            let mut d = Document::parse(src.as_str()).unwrap();
            d.root_mut().unwrap().move_entry(1, 0).unwrap();
            assert_eq!(d.to_string(), n(&format!("{last}a: 1\n# footer\n")));

            let mut d = Document::parse(src.as_str()).unwrap();
            d.set_path(&path!["new"], 3).unwrap();
            assert_eq!(d.to_string(), n(&format!("a: 1\n{last}new: 3\n# footer\n")));
            reparses(&d, &src);
        }
    }
}

/// Comment rule 4 and choice C6 of the rulebook (replaces the
/// provisional 0.2 test "middle entry block scalar comments still
/// travel"): the comment lines after the block scalar of a **middle**
/// entry lead the next entry. Removing the entry leaves them; moving it
/// leaves them with the next entry.
#[test]
fn middle_entry_block_scalar_comments_lead_the_next_entry() {
    for nl in NLS {
        let n = |s: &str| s.replace('\n', nl);
        let src = n("k: |\n  text\n# c\nm: 2\n");
        let mut d = Document::parse(src.as_str()).unwrap();
        let r = d.remove_path(&path!["k"]).unwrap();
        assert!(r.comments().is_empty(), "{:?}", r.comments());
        assert_eq!(d.to_string(), n("# c\nm: 2\n"));
        reparses(&d, &src);
        let mut d = Document::parse(src.as_str()).unwrap();
        d.root_mut().unwrap().move_entry(0, 1).unwrap();
        assert_eq!(d.to_string(), n("# c\nm: 2\nk: |\n  text\n"));
        reparses(&d, &src);
        // Moving `m` carries its leading comment line.
        let mut d = Document::parse(src.as_str()).unwrap();
        d.root_mut().unwrap().move_entry(1, 0).unwrap();
        assert_eq!(d.to_string(), n("# c\nm: 2\nk: |\n  text\n"));
        reparses(&d, &src);
    }
}

/// The 0.2 limitation class of atomic corner shapes (block
/// scalars next to comments and blank lines),
/// once refused as `NotWellFormed`, now follow the rulebook: each comment
/// keeps its owner (rule 4: the comment above `m` leads it and moves with
/// it; rules 5 and 7 keep `# a` and `# d`), with every line break kind.
/// Moving an entry to the top of a document drops the blank lines between
/// its leading comments and it: there a blank line would make them the
/// document header (rules 2 and 3); `|+` blank lines are content and stay.
#[test]
fn block_scalar_comment_corner_shapes_follow_the_rulebook() {
    let cases: &[(&str, usize, usize, &str)] = &[
        (
            "k: 1 # a\n# c\nm: |\n  x\n# d\n",
            1,
            0,
            "# c\nm: |\n  x\nk: 1 # a\n# d\n",
        ),
        // No final line break: the moved block scalar ends the text with
        // its own.
        ("k: |\n  text\n# c\nm: 2", 0, 1, "# c\nm: 2\nk: |\n  text\n"),
        (
            "k: >\n  text\n\n# c\n\nm: 2\n",
            1,
            0,
            "# c\nm: 2\nk: >\n  text\n",
        ),
        // `|+` with blank lines, re-inserted by a move.
        (
            "k: 1\n\n# c\n\nm: |+\n  a\n\n",
            1,
            0,
            "# c\nm: |+\n  a\n\nk: 1\n",
        ),
        (
            "- 1\n# c\n\n- |+\n  a\n\n\n",
            1,
            0,
            "# c\n- |+\n  a\n\n\n- 1\n",
        ),
    ];
    for nl in NLS {
        for (src, from, to, want) in cases {
            let (src, want) = (src.replace('\n', nl), want.replace('\n', nl));
            let mut d = Document::parse(src.as_str()).unwrap();
            let before = Document::parse(src.as_str()).unwrap().to_value().unwrap();
            d.root_mut()
                .unwrap()
                .move_entry(*from, *to)
                .unwrap_or_else(|e| panic!("{src:?} {from}->{to}: {e:?}"));
            assert_eq!(d.to_string(), want, "{src:?} {from}->{to}");
            reparses(&d, &src);
            let v = d.to_value().unwrap();
            if let (Some(a), Some(b)) = (v.as_sequence(), before.as_sequence()) {
                // The items, moved as the
                // call says (item `from` now at `to`, the rest in order).
                let mut want: Vec<_> = b.iter().cloned().collect();
                let moved = want.remove(*from);
                want.insert(*to, moved);
                let got: Vec<_> = a.iter().cloned().collect();
                assert_eq!(got, want, "{src:?} {from}->{to}");
            } else {
                assert_eq!(v, before, "{src:?}");
            }
        }
    }
}
