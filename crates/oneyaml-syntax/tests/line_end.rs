//! Edits at a line's end (block
//! scalars replaced or inserted at the end of a collection or document;
//! New multi-line strings are `|` blocks).
//!
//! Every chomping (`|`, `|-`, `|+`, `>`) and plain content, replaced by
//! and replacing each other, and inserted after each other, at the end of
//! a block mapping, a block sequence, a nested collection, a document, a
//! document ended by `...`, and a document followed by another, with
//! trailing comments and blank lines after them. Each result is the
//! string splice of the edit, satisfies every read invariant, and equals
//! its own re-parse.
//!
//! §5.4 (CR, LF and CRLF are all `b-break` on the
//! edit path): every test runs over LF, CRLF and CR documents and
//! fragments.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod edit_support;

use edit_support::{invariants, reparses};
use oneyaml_core::Settings;
use oneyaml_syntax::tree::{Fragment, Node, SyntaxKind as K, SyntaxTree, TreeEditError};

/// Values that end their line, as written after `key: ` or `- ` with
/// their content indented to `ind` columns: (name, text). The text of a
/// block scalar includes its line break; plain text is followed by one.
/// Every line ends with `eol`.
fn values(ind: usize, eol: &str) -> Vec<(&'static str, String)> {
    let p = " ".repeat(ind);
    let v = vec![
        ("literal", format!("|\n{p}x\n{p}y\n")),
        ("strip", format!("|-\n{p}x\n")),
        ("keep", format!("|+\n{p}x\n\n")),
        ("folded", format!(">\n{p}x\n{p}y\n")),
        ("plain", "1\n".to_owned()),
        ("flow", "[a, b]\n".to_owned()),
    ];
    v.into_iter().map(|(n, t)| (n, nl(&t, eol))).collect()
}

/// The three line breaks.
const EOLS: [&str; 3] = ["\n", "\r\n", "\r"];

/// `s` with every `\n` written as `eol`.
fn nl(s: &str, eol: &str) -> String {
    s.replace('\n', eol)
}

/// What may follow the edited line.
const TRAILERS: &[&str] = &["", "# c\n", "\n", "\n\n# c\n", "# c\n\n"];

/// (name, text before the value, text after the value's line, value
/// indentation, the value is a mapping value). Every line ends with
/// `eol`.
fn contexts(eol: &str) -> Vec<(&'static str, String, String, usize, bool)> {
    let v: Vec<(&'static str, &'static str, &'static str, usize, bool)> = vec![
        ("mapping end", "k: 0\na: ", "", 2, true),
        ("sequence end", "- 0\n- ", "", 2, false),
        ("nested mapping end", "o:\n  k: 0\n  a: ", "", 4, true),
        ("nested sequence end", "o:\n- 0\n- ", "", 2, false),
        ("mapping before outer entry", "o:\n  a: ", "z: 9\n", 4, true),
        ("document root", "", "", 1, false),
        ("after ---", "--- ", "", 1, false),
        ("document then ...", "--- ", "...\n", 1, false),
        ("mapping then ...", "a: ", "...\n", 2, true),
        ("document then another", "--- ", "--- b\n", 1, false),
        ("mapping then another", "a: ", "--- b\n", 2, true),
        ("sequence in stream", "--- a\n--- \n- ", "--- c\n", 2, false),
    ];
    v.into_iter()
        .map(|(n, b, a, i, m)| (n, nl(b, eol), nl(a, eol), i, m))
        .collect()
}

/// The value node that starts at byte `at` (the outermost one).
fn value_at(tree: &SyntaxTree, at: usize) -> Node<'_> {
    tree.root()
        .descendants()
        .find(|n| n.kind().is_yaml_node() && n.span().start == at && !n.span().is_empty())
        .unwrap_or_else(|| panic!("no node at {at} in {:?}", tree.emit()))
}

/// `text` (a value that ends its line) as a fragment of its own.
fn value_fragment(text: &str) -> Fragment {
    Fragment::parse(text).unwrap()
}

fn check(tree: &SyntaxTree, expected: &str, what: &str) {
    assert_eq!(tree.emit(), expected, "{what}");
    invariants(tree);
    reparses(tree, &Settings::new());
}

#[test]
fn replace_values_that_end_their_line() {
    for eol in EOLS {
        replace_values_that_end_their_line_with(eol);
    }
}

fn replace_values_that_end_their_line_with(eol: &str) {
    let mut cases = 0;
    let mut refused = 0;
    for (ctx, before, after, ind, _) in contexts(eol) {
        for trailer in TRAILERS {
            // A trailer that is not a comment or blank line would change
            // how the context's last line reads; `...`/`---` come after.
            let rest = format!("{}{after}", nl(trailer, eol));
            for (old_name, old) in values(ind, eol) {
                for (new_name, new) in values(ind, eol) {
                    let src = format!("{before}{old}{rest}");
                    let tree0 = SyntaxTree::parse(src.as_str());
                    if tree0.has_errors() {
                        continue;
                    }
                    let mut tree = tree0.clone();
                    let target = value_at(&tree, before.len());
                    // The old value as written (a keep scalar followed by
                    // blank lines takes them: another shape, skipped).
                    if target.text() != own_text(&old) {
                        continue;
                    }
                    let id = target.id();
                    let what = format!("{ctx}: {old_name} -> {new_name}, {src:?}");
                    let before = before.as_str();
                    let expected = format!("{before}{new}{rest}");
                    if !reads_as(&expected, before.len(), &new) {
                        // The text would read otherwise (a keep scalar
                        // before blank lines): refused, nothing changed.
                        let r = tree.replace_to_line_end(id, id, value_fragment(&new));
                        assert!(r.is_err(), "{what}: accepted {:?}", tree.emit());
                        assert_eq!(tree.emit(), src, "{what}");
                        refused += 1;
                        continue;
                    }
                    let r = tree
                        .replace_to_line_end(id, id, value_fragment(&new))
                        .unwrap_or_else(|e| panic!("{e:?}: {what}"));
                    check(&tree, &expected, &what);
                    assert_eq!(r.removed.text(), &*target_text(&tree0, before.len()));
                    let new_node = tree.get(r.inserted.last).unwrap();
                    assert!(new_node.kind().is_yaml_node(), "{what}");
                    assert_eq!(new_node.span().start, before.len(), "{what}");
                    assert!(tree.get(id).is_err(), "{what}: the old node is stale");
                    cases += 1;
                }
            }
        }
    }
    assert!(cases > 1000 && refused > 0, "{eol:?}: {cases} {refused}");
}

/// `text` parses cleanly and has a node at `at` whose text is `value`
/// (without a plain value's line break).
fn reads_as(text: &str, at: usize, value: &str) -> bool {
    let t = SyntaxTree::parse(text);
    if t.has_errors() {
        return false;
    }
    let want = own_text(value);
    t.root()
        .descendants()
        .any(|n| n.kind().is_yaml_node() && n.span().start == at && n.text() == want)
}

/// The text of the node a value is: a block scalar's with its line
/// breaks, other content's without its line break.
fn own_text(value: &str) -> &str {
    if value.starts_with(['|', '>']) {
        value
    } else {
        value.trim_end_matches(['\n', '\r'])
    }
}

fn target_text(tree: &SyntaxTree, at: usize) -> String {
    value_at(tree, at).text().into_owned()
}

#[test]
fn insert_entries_after_values_that_end_their_line() {
    for eol in EOLS {
        insert_entries_after_values_that_end_their_line_with(eol);
    }
}

fn insert_entries_after_values_that_end_their_line_with(eol: &str) {
    let mut cases = 0;
    for (ctx, before, after, ind, map) in contexts(eol) {
        let before = before.as_str();
        // Entries go into the collection the value is in.
        let Some(line_start) = before.rfind(['\n', '\r']).map(|i| i + 1).or(Some(0)) else {
            continue;
        };
        let lead = &before[line_start..];
        let col = lead.len() - lead.trim_start().len();
        let is_entry = if map {
            lead.trim_start().ends_with(": ") && !lead.starts_with("---")
        } else {
            lead.trim_start() == "- "
        };
        if !is_entry {
            continue;
        }
        let pad = " ".repeat(col);
        for trailer in TRAILERS {
            let rest = format!("{}{after}", nl(trailer, eol));
            for (old_name, old) in values(ind, eol) {
                for (new_name, new) in values(ind, eol) {
                    let src = format!("{before}{old}{rest}");
                    let tree0 = SyntaxTree::parse(src.as_str());
                    if tree0.has_errors() {
                        continue;
                    }
                    let mut tree = tree0.clone();
                    let id = value_at(&tree, before.len()).id();
                    // The new entry, parsed at its column and unwrapped.
                    let entry = if map {
                        format!("{pad}b: {new}")
                    } else {
                        format!("{pad}- {new}")
                    };
                    let f = Fragment::parse(&entry).unwrap();
                    let f = if col > 0 {
                        // Leading indentation, then the collection.
                        let kinds: Vec<K> = f.root_kinds().collect();
                        assert_eq!(kinds[0], K::Whitespace, "{entry:?}");
                        f.unwrap_root(1).unwrap()
                    } else {
                        f.unwrap_root(0).unwrap()
                    };
                    let what = format!("{ctx}: after {old_name} insert {new_name}, {src:?}");
                    let target = value_at(&tree0, before.len());
                    if target.text() != own_text(&old) {
                        continue;
                    }
                    let expected = format!("{before}{old}{entry}{rest}");
                    let at = before.len() + old.len() + entry.len() - new.len();
                    if !reads_as(&expected, at, &new) {
                        let r = tree.insert_line_after(id, f);
                        assert!(r.is_err(), "{what}: accepted {:?}", tree.emit());
                        assert_eq!(tree.emit(), src, "{what}");
                        continue;
                    }
                    let ins = tree
                        .insert_line_after(id, f)
                        .unwrap_or_else(|e| panic!("{e:?}: {what}"));
                    check(&tree, &expected, &what);
                    let last = tree.get(ins.last).unwrap();
                    assert!(last.kind().is_yaml_node(), "{what}: {last:?}");
                    assert_eq!(tree.get(id).unwrap().span().start, before.len());
                    cases += 1;
                }
            }
        }
    }
    assert!(cases > 500, "{eol:?}: {cases}");
}

/// Replacing and inserting at the end of the text without a final line
/// break, and the refusals (with each line break).
#[test]
fn line_end_edge_cases_and_refusals() {
    for eol in EOLS {
        line_end_edge_cases_and_refusals_with(eol);
    }
}

fn line_end_edge_cases_and_refusals_with(eol: &str) {
    let n = |s: &str| nl(s, eol);
    let frag = |s: &str| value_fragment(&n(s));
    // The end of the text ends the line.
    let mut t = SyntaxTree::parse("a: 1");
    let one = value_at(&t, 3).id();
    let _ = t.replace_to_line_end(one, one, frag("|\n  x\n")).unwrap();
    check(&t, &n("a: |\n  x\n"), "eof");
    let mut t = SyntaxTree::parse("a");
    let a = value_at(&t, 0).id();
    let _ = t.replace_to_line_end(a, a, frag("|-\n x\n")).unwrap();
    check(&t, &n("|-\n x\n"), "root eof");
    // A line that goes on after the range.
    let mut t = SyntaxTree::parse(n("a: 1 # c\n"));
    let one = value_at(&t, 3).id();
    assert_eq!(
        t.replace_to_line_end(one, one, frag("2\n")).err(),
        Some(TreeEditError::NotWellFormed)
    );
    assert_eq!(t.emit(), n("a: 1 # c\n"));
    assert!(!t.is_edited());
    // A fragment that does not end its line.
    let mut t = SyntaxTree::parse(n("a: |\n  x\n"));
    let v = value_at(&t, 3).id();
    assert_eq!(
        t.replace_to_line_end(v, v, value_fragment("2")).err(),
        Some(TreeEditError::NotWellFormed)
    );
    // Only a line break.
    assert_eq!(
        t.replace_to_line_end(v, v, Fragment::parse(eol).unwrap())
            .err(),
        Some(TreeEditError::EmptyFragment)
    );
    // Content that does not fit: a block scalar indented less than its
    // parent needs is refused and the tree is unchanged.
    let src = n("o:\n  a: |\n    x\nz: 1\n");
    let mut t = SyntaxTree::parse(src.as_str());
    let v = value_at(&t, src.find('|').unwrap()).id();
    let before = t.emit();
    let r = t.replace_to_line_end(v, v, frag("|\nx\n"));
    assert!(r.is_err(), "{r:?}");
    assert_eq!(t.emit(), before);
    // Insertion after a value at the very end without a line break.
    let mut t = SyntaxTree::parse("a: 1");
    let one = value_at(&t, 3).id();
    let f = Fragment::parse(&n("b: 2\n"))
        .unwrap()
        .unwrap_root(0)
        .unwrap();
    assert_eq!(
        t.insert_line_after(one, f).err(),
        Some(TreeEditError::NotWellFormed)
    );
    // Stale handles.
    let mut t = SyntaxTree::parse(n("a: |\n  x\n"));
    let v = value_at(&t, 3).id();
    let _ = t.replace_to_line_end(v, v, frag("1\n")).unwrap();
    assert!(matches!(
        t.replace_to_line_end(v, v, frag("1\n")),
        Err(TreeEditError::Stale(_))
    ));
    check(&t, &n("a: 1\n"), "stale");
}

/// Handles outside the edit keep resolving, with their text, through
/// both splices of a two-part edit, and after a refusal (with
/// each line break).
#[test]
fn handles_survive_two_part_edits() {
    for eol in EOLS {
        handles_survive_two_part_edits_with(eol);
    }
}

fn handles_survive_two_part_edits_with(eol: &str) {
    let n = |s: &str| nl(s, eol);
    let src = n("p: [q]\no:\n  a: |\n    x\n# c\nz: 1\n");
    let mut t = SyntaxTree::parse(src.as_str());
    let (head, tail) = (src.find("o:").unwrap(), src.find("# c").unwrap());
    let keep: Vec<_> = t
        .root()
        .descendants()
        .filter(|x| x.span().end <= head || x.span().start >= tail)
        .map(|x| (x.id(), x.text().into_owned()))
        .collect();
    let v = value_at(&t, src.find('|').unwrap()).id();
    let r = t
        .replace_to_line_end(v, v, value_fragment(&n("true\n")))
        .unwrap();
    check(&t, &n("p: [q]\no:\n  a: true\n# c\nz: 1\n"), "two-part");
    for (id, text) in &keep {
        if let Ok(x) = t.get(*id) {
            assert_eq!(x.text(), text.as_str());
        } else {
            // Only the old line break (moved across levels) may go.
            assert_eq!(text, eol);
        }
    }
    // Back to a block scalar, by the new node's handle.
    let new = r.inserted.last;
    let _ = t
        .replace_to_line_end(new, new, value_fragment(&n("|+\n    x\n\n")))
        .unwrap();
    check(&t, &n("p: [q]\no:\n  a: |+\n    x\n\n# c\nz: 1\n"), "back");
}

/// Bug 3: root and document-level replacements in every shape
/// are accepted exactly when the spliced text reads as the new root
/// (never a false refusal), and refused without a change otherwise. Roots
/// (block mapping, block sequence at columns 0 and 1, plain, quoted,
/// flow, literal, folded) replaced by each other, bare, after `---` (on
/// its line and on the next), after `%YAML`, before `...`, and before
/// another document; through the line end (`replace_to_line_end`) and,
/// for content that does not end its line, in place (`replace`).
#[test]
fn root_replacements_in_every_shape() {
    for eol in EOLS {
        root_replacements_with(eol);
    }
}

fn root_replacements_with(eol: &str) {
    let n = |s: &str| nl(s, eol);
    // (text at column `col`, first line unindented; can share the `---`
    // line).
    let roots = |p: &str| -> Vec<(String, bool)> {
        let v = vec![
            (format!("a: 1\n{p}b: 2"), false),
            (format!("- a\n{p}- b"), false),
            ("z".to_owned(), true),
            ("'q'".to_owned(), true),
            ("\"d\"".to_owned(), true),
            ("[a, {b: c}]".to_owned(), true),
            (format!("|\n{p} x\n"), true),
            (format!(">-\n{p} f\n"), true),
        ];
        v.into_iter().map(|(t, i)| (n(&t), i)).collect()
    };
    let wrappers: &[(&str, &str, bool)] = &[
        ("", "", false),
        ("---\n", "", false),
        ("--- ", "", true),
        ("%YAML 1.2\n---\n", "", false),
        ("", "...\n", false),
        ("--- ", "...\n", true),
        ("", "--- b\n", false),
        ("--- a\n---\n", "--- c\n", false),
        ("# c\n", "# d\n", false),
    ];
    let (mut accepted, mut refused) = (0, 0);
    for &(head, tail, same_line) in wrappers {
        let (head, tail) = (n(head), n(tail));
        let (head, tail) = (head.as_str(), tail.as_str());
        for col in [0usize, 1] {
            if same_line && col > 0 {
                continue;
            }
            let p = " ".repeat(col);
            for (old, old_inline) in roots(&p) {
                if same_line && !old_inline {
                    continue;
                }
                let old_line = if old.ends_with(['\n', '\r']) {
                    old.clone()
                } else {
                    format!("{old}{eol}")
                };
                let src = format!("{head}{p}{old_line}{tail}");
                let tree0 = SyntaxTree::parse(src.as_str());
                if tree0.has_errors() {
                    continue;
                }
                let at = head.len() + col;
                let target = value_at(&tree0, at);
                assert_eq!(target.text(), own_text(&old), "{src:?}");
                for (new, _) in roots(&p) {
                    let new_line = if new.ends_with(['\n', '\r']) {
                        new.clone()
                    } else {
                        format!("{new}{eol}")
                    };
                    // The new root as a node fragment, parsed in place.
                    let ft = SyntaxTree::parse(format!("{head}{p}{new_line}"));
                    if ft.has_errors() {
                        continue;
                    }
                    let n = value_at(&ft, at);
                    let node = ft.extract(n.id(), n.id()).unwrap();
                    let expected = format!("{head}{p}{new_line}{tail}");
                    let ok = reads_as(&expected, at, &new);
                    let what = format!("{src:?} <- {new:?}");
                    // Through the line end.
                    let mut t = tree0.clone();
                    let id = value_at(&t, at).id();
                    let f = if new.ends_with(['\n', '\r']) {
                        node.clone()
                    } else {
                        node.clone().concat(Fragment::parse(eol).unwrap()).unwrap()
                    };
                    match t.replace_to_line_end(id, id, f) {
                        Ok(_) => {
                            assert!(ok, "{what}: accepted {:?}", t.emit());
                            check(&t, &expected, &what);
                            accepted += 1;
                        }
                        Err(e) => {
                            assert!(!ok, "{what}: false refusal {e:?}");
                            assert_eq!(t.emit(), src);
                            refused += 1;
                        }
                    }
                    // In place, when neither ends its line.
                    if !old.ends_with(['\n', '\r']) && !new.ends_with(['\n', '\r']) {
                        let mut t = tree0.clone();
                        let id = value_at(&t, at).id();
                        match t.replace(id, node) {
                            Ok(_) => {
                                assert!(ok, "{what}: accepted {:?}", t.emit());
                                check(&t, &expected, &what);
                            }
                            Err(e) => {
                                assert!(!ok, "{what}: false refusal (replace) {e:?}");
                                assert_eq!(t.emit(), src);
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("root replacements ({eol:?}): {accepted} accepted, {refused} refused");
    assert!(
        accepted > 300,
        "{eol:?}: {accepted} accepted, {refused} refused"
    );
}

/// The children of the `n`-th block collection of `t`.
fn coll_kids(t: &SyntaxTree, n: usize) -> Vec<Node<'_>> {
    t.root()
        .descendants()
        .filter(|x| matches!(x.kind(), K::BlockMapping | K::BlockSequence))
        .nth(n)
        .unwrap()
        .children()
        .collect()
}

/// For real-world files: `remove_to_line_end` removes the
/// last entry of a block collection whose value ends its line (a block
/// scalar), putting the previous line's end where the parser puts it
/// (after the collection, C4); with `None`, a plain last entry after a
/// kept `|+` scalar goes with its line break (outside the collection) in
/// one edit. Every result equals its re-parse; refusals change nothing.
#[test]
fn removals_through_the_line_end() {
    for eol in ["\n", "\r\n", "\r"] {
        let brk = || Some(Fragment::parse(eol).unwrap());
        // (source, first and last child removed, trivia, result)
        let cases: Vec<(String, usize, usize, Option<Fragment>, String)> = vec![
            (
                nl("k: 1\nm: |\n  t2\n", eol),
                4,
                8,
                brk(),
                nl("k: 1\n", eol),
            ),
            (nl("- a\n- |\n  b\n", eol), 3, 6, brk(), nl("- a\n", eol)),
            (
                nl("k: 1 # x\nm: |\n  t2\n# c\n", eol),
                4,
                10,
                Some(Fragment::parse(&nl(" # x\n", eol)).unwrap()),
                nl("k: 1 # x\n# c\n", eol),
            ),
            (
                nl("a: 1\nk: |\n  t\n---\nb: 2\n", eol),
                4,
                8,
                brk(),
                nl("a: 1\n---\nb: 2\n", eol),
            ),
        ];
        for (src, a, b, trivia, want) in cases {
            let mut t = SyntaxTree::parse(src.as_str());
            let (first, last) = {
                let kids = coll_kids(&t, 0);
                (kids[a].id(), kids[b].id())
            };
            let removed = t.remove_to_line_end(first, last, trivia).unwrap();
            assert!(!removed.text().is_empty());
            check(&t, &want, &src);
        }
        // `None`: the entry's own break, outside the inner mapping, goes.
        let src = nl("o:\n  k0: v0\n  k1: |+\n    x\n\n  k2: v2\nz: 1\n", eol);
        let mut t = SyntaxTree::parse(src.as_str());
        let (first, last) = {
            let kids = coll_kids(&t, 1);
            let block = kids.iter().position(|k| k.block_header().is_some());
            (kids[block.unwrap() + 1].id(), kids.last().unwrap().id())
        };
        let _ = t.remove_to_line_end(first, last, None).unwrap();
        check(
            &t,
            &nl("o:\n  k0: v0\n  k1: |+\n    x\n\nz: 1\n", eol),
            &src,
        );
        // Refused, the tree unchanged: trivia that is not only trivia, or
        // does not end its line; a range whose line goes on after it.
        let src = nl("k: 1\nm: |\n  t2\n", eol);
        let mut t = SyntaxTree::parse(src.as_str());
        let (first, last, k, one) = {
            let kids = coll_kids(&t, 0);
            (kids[4].id(), kids[8].id(), kids[0].id(), kids[2].id())
        };
        for bad in [nl("x\n", eol), "# c".to_owned()] {
            let f = Fragment::parse(&bad).unwrap();
            let r = t.remove_to_line_end(first, last, Some(f));
            assert_eq!(r.err(), Some(TreeEditError::NotWellFormed), "{bad:?}");
        }
        let r = t.remove_to_line_end(k, one, None);
        assert_eq!(r.err(), Some(TreeEditError::NotWellFormed));
        assert_eq!(t.emit(), src);
        assert!(!t.is_edited());
        // Comment lines inserted after a root collection that ends with a
        // block scalar go at stream level (`insert_line_after`
        // puts trivia lines at the parser's place).
        let src = nl("k: |\n  t\n---\nb: 2\n", eol);
        let mut t = SyntaxTree::parse(src.as_str());
        let m = t
            .root()
            .descendants()
            .find(|n| n.kind() == K::BlockMapping)
            .unwrap()
            .id();
        let lines = Fragment::parse(&nl("# c1\n# c2\n", eol)).unwrap();
        let ins = t.insert_line_after(m, lines).unwrap();
        let parent = t.get(ins.first).unwrap().parent().unwrap().kind();
        assert_eq!(parent, K::Stream);
        check(&t, &nl("k: |\n  t\n# c1\n# c2\n---\nb: 2\n", eol), &src);
    }
}
