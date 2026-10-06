//! Memory retained after edits follows
//! the tree, not the edit history; edit cost is independent of edit
//! history; validation cost is local; a lenient warning leaves with the
//! text its labels are in.
//!
//! Timing method as in `edit_history.rs` and `edit_scaling.rs`: every
//! comparison alternates the small and the large case, each keeping its
//! minimum over several runs on fresh copies (copying and locating the
//! targets are not timed), so a burst of load on the host slows both.
//! Each bound sits between the flat ratio (~1, or ~1-2 with cache effects
//! of a larger tree) and the ratio of the defect (≥ 10). Every test here
//! fails on a tree that keeps its edit history.
//!
//! Reported with `--nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use oneyaml_core::Settings;
use oneyaml_core::code::LENIENT_INLINE_COLON;
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxKind as K, SyntaxTree, TreeEditError};

/// The timed tests run one at a time.
static TIMING: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs `small` and `large` alternately `rounds` times, each on a fresh
/// `setup_*()` (not timed), and returns the minimum time of each.
fn alternate<A, B>(
    rounds: usize,
    setup_small: impl Fn() -> A,
    small: impl Fn(&mut A),
    setup_large: impl Fn() -> B,
    large: impl Fn(&mut B),
) -> (Duration, Duration) {
    let (mut a, mut b) = (Duration::MAX, Duration::MAX);
    for _ in 0..rounds {
        let mut x = setup_small();
        let start = Instant::now();
        small(&mut x);
        a = a.min(start.elapsed());
        let mut y = setup_large();
        let start = Instant::now();
        large(&mut y);
        b = b.min(start.elapsed());
    }
    (a, b)
}

fn ratio(small: Duration, large: Duration) -> f64 {
    large.as_secs_f64() / small.as_secs_f64().max(1e-9)
}

/// The three line breaks (§5.4 `b-break`): every test whose cost or
/// acceptance depends on line breaks runs over each (the fast
/// paths once accepted only `\n`, so CR-only documents took the
/// slow path).
const EOLS: [&str; 3] = ["\n", "\r\n", "\r"];

/// `s` with every `\n` written as `eol`.
fn nl(s: &str, eol: &str) -> String {
    s.replace('\n', eol)
}

/// A name for messages.
fn eol_name(eol: &str) -> &'static str {
    match eol {
        "\n" => "LF",
        "\r\n" => "CRLF",
        _ => "CR",
    }
}

/// The plain scalar token whose text is `text`.
fn plain(t: &SyntaxTree, text: &str) -> NodeId {
    t.root()
        .descendants()
        .find(|x| x.kind() == K::PlainScalar && x.text() == text)
        .unwrap()
        .id()
}

/// 64 replacements of token `id` by `9` and `b` in turn.
fn flips(t: &mut SyntaxTree, id: &mut NodeId) {
    for i in 0..64 {
        *id = t
            .replace_token(*id, if i % 2 == 0 { "9" } else { "b" })
            .unwrap();
    }
}

// =================== a fork after many toggles copies the tree not the history

/// `n` toggles of the first plain scalar between `x` and `y`.
fn toggled(src: &str, n: usize) -> SyntaxTree {
    let mut t = SyntaxTree::parse(src);
    let mut id = t
        .root()
        .descendants()
        .find(|x| x.kind() == K::PlainScalar)
        .unwrap()
        .id();
    for i in 0..n {
        id = t
            .replace_token(id, if i % 2 == 0 { "x" } else { "y" })
            .unwrap();
    }
    t
}

/// Memory: after 100k token toggles, a fork copies what
/// the tree holds, not the edits' history (before the fix: 25.9 MB copied,
/// 16-23 ms, against 64 µs to parse), on a 15-byte and a 3 KB document.
/// The retained bytes themselves are counted in `edit.rs`
/// (`toggles_retain_what_the_tree_holds_not_the_history`).
#[test]
fn a_fork_after_many_toggles_copies_the_tree_not_the_history() {
    let _serial = serial();
    let big: String = (0..300).map(|i| format!("key{i}: v\n")).collect();
    for src in ["a: [q, y]\nb: 1\n".to_owned(), format!("a: q\n{big}")] {
        let few = toggled(&src, 100);
        let many = toggled(&src, 100_000);
        assert_eq!(few.emit(), many.emit());
        let fork = |t: &SyntaxTree| {
            let start = Instant::now();
            for _ in 0..16 {
                std::hint::black_box(t.fork());
            }
            start.elapsed()
        };
        let (mut a, mut b) = (Duration::MAX, Duration::MAX);
        for _ in 0..9 {
            a = a.min(fork(&few));
            b = b.min(fork(&many));
        }
        let r = ratio(a, b);
        eprintln!(
            "fork of a {}-byte tree after 100 toggles: {a:?}, after 100k: {b:?}, ratio {r:.2}",
            src.len()
        );
        assert!(r < 3.0, "a fork copies the edit history: {r:.2}×");
        // The copy is the tree, and handles resolve in both.
        let copy = many.fork();
        assert_eq!(copy, SyntaxTree::parse(many.emit()));
        let id = plain(&many, "q");
        assert_eq!(copy.get(id).unwrap().text(), "q");
    }
}

// ========================= stream trivia inserted by many edits is jumped over

/// `a: 1`, then `--- b` after `k` comment lines that `k` edits inserted
/// one at a time (each its own small arena), forked for each run; every
/// line ended by `eol`.
fn after_inserted_comments(k: usize, eol: &str) -> SyntaxTree {
    let mut t = SyntaxTree::parse(nl("a: 1\n--- b\n", eol));
    let doc = t
        .root()
        .children()
        .filter(|x| x.kind() == K::Document)
        .nth(1);
    let doc = doc.unwrap().id();
    let line = Fragment::parse(&nl("# d\n", eol)).unwrap();
    for _ in 0..k {
        let _ = t.insert_before(doc, line.clone()).unwrap();
    }
    assert!(!t.has_errors());
    t
}

/// Edit cost is independent of edit history:
/// `b` replaced 64 times after `k` comment lines inserted by `k` edits.
/// The walk back to the document before stepped over each inserted
/// arena; runs that are all stream-level trivia are now jumped
/// over by the treap's sums.
///
/// §5.4 (CR, LF and CRLF are all `b-break` on the edit path): the same over
/// CRLF and CR documents, and the CR and CRLF builds cost what the LF build
/// costs (before the fix, the trivia shortcut took only a `\n`-ended line break
/// as the edit's own, so 10k CR insertions took 62.4 s against 45 ms for LF).
#[test]
fn stream_trivia_inserted_by_many_edits_is_jumped_over() {
    let _serial = serial();
    let mut lf_build = None;
    for eol in EOLS {
        let name = eol_name(eol);
        // The insertions themselves: each comment line went in right
        // before `--- b`, and the trivia shortcut refused it (no line break
        // of the tree's own lay between it and the document), so each
        // re-read `a: 1` and every comment line inserted so far: 10k
        // insertions took 54 s.
        let (mut build_small, mut build_large) = (Duration::MAX, Duration::MAX);
        for _ in 0..3 {
            let start = Instant::now();
            std::hint::black_box(after_inserted_comments(1_000, eol));
            build_small = build_small.min(start.elapsed());
            let start = Instant::now();
            std::hint::black_box(after_inserted_comments(10_000, eol));
            build_large = build_large.min(start.elapsed());
        }
        let per = ratio(build_small, build_large) / 10.0;
        eprintln!(
            "{name}: 1k comment lines inserted before a document: {build_small:?}, 10k: \
             {build_large:?}, per-insertion ratio {per:.2} (O(k) each ~5.5)"
        );
        assert!(
            per < 2.5,
            "{name}: insertion cost grew with the insertions: {per:.2}×"
        );
        // The same script costs the same whatever the line break.
        let lf = *lf_build.get_or_insert(build_large);
        let across = ratio(lf, build_large);
        eprintln!("{name}: 10k insertions cost {across:.2}× the LF ones");
        assert!(
            across < 3.0,
            "{name}: 10k insertions cost {across:.2}× the LF ones"
        );
        let small = after_inserted_comments(1_000, eol);
        let large = after_inserted_comments(10_000, eol);
        let setup = |t: &SyntaxTree| {
            let f = t.fork();
            let id = plain(&f, "b");
            (f, id)
        };
        let run = |(t, id): &mut (SyntaxTree, NodeId)| flips(t, id);
        let (a, b) = alternate(5, || setup(&small), run, || setup(&large), run);
        let r = ratio(a, b);
        eprintln!(
            "{name}: 64 edits after 1k inserted comment lines: {a:?}, after 10k: {b:?}, ratio \
             {r:.2}"
        );
        assert!(
            r < 2.5,
            "{name}: edit cost grew with the edit history: {r:.2}×"
        );
        // The edited tree is the tree of its text.
        let (mut t, mut id) = setup(&large);
        flips(&mut t, &mut id);
        assert_eq!(t, SyntaxTree::parse(t.emit()), "{name}");
    }
}

// =================================== edits before trailing comments stay local

/// `--- b` followed by `n` comment lines, parsed.
fn before_parsed_comments(n: usize) -> SyntaxTree {
    SyntaxTree::parse(format!("--- b\n{}", "# c\n".repeat(n)))
}

/// `--- b` followed by `n` comment lines that `n` edits appended.
fn before_inserted_comments(n: usize) -> SyntaxTree {
    let mut t = SyntaxTree::parse("--- b\n");
    let root = t.root().id();
    let line = Fragment::parse("# c\n").unwrap();
    for _ in 0..n {
        let _ = t.append_child(root, line.clone()).unwrap();
    }
    t
}

/// An edit of the last document walked every
/// stream-level comment after it, one element at a time, to find what
/// follows; the walk now jumps to the next content.
#[test]
fn edits_before_trailing_comments_stay_local() {
    let _serial = serial();
    for (what, make, n) in [
        (
            "parsed",
            before_parsed_comments as fn(usize) -> SyntaxTree,
            100_000,
        ),
        ("inserted by edits", before_inserted_comments, 10_000),
    ] {
        let small = make(1_000);
        let large = make(n);
        let setup = |t: &SyntaxTree| {
            let f = t.fork();
            let id = plain(&f, "b");
            (f, id)
        };
        let run = |(t, id): &mut (SyntaxTree, NodeId)| flips(t, id);
        let (a, b) = alternate(5, || setup(&small), run, || setup(&large), run);
        let r = ratio(a, b);
        eprintln!(
            "64 edits before 1k comment lines ({what}): {a:?}, before {n}: {b:?}, ratio {r:.2}"
        );
        assert!(r < 3.0, "{what}: edit cost grew with the comments: {r:.2}×");
        let (mut t, mut id) = setup(&large);
        flips(&mut t, &mut id);
        assert_eq!(t, SyntaxTree::parse(t.emit()));
    }
}

// ============================ appending after a final block scalar stays local

/// A root mapping of about `bytes` bytes whose last value is a folded
/// block scalar (its token holds the text's final line break, so the
/// document is the stream's only child): the gha-workflow shape, every
/// line ended by `eol`.
fn ends_in_block_scalar(bytes: usize, eol: &str) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < bytes {
        s.push_str(&format!("job{i}:\n  runs-on: ubuntu\n  steps: [a, b]\n"));
        i += 1;
    }
    s.push_str("last: >-\n  folded\n  text\n");
    nl(&s, eol)
}

/// The gha-workflow outlier: `insert_line_after(<last
/// value>, "k: x\n")` puts the new entry in the mapping and its line
/// break after the document, at stream level; that splice's window read
/// the whole document, as did
/// a comment appended after such a file. Both are now
/// read with the document's tail.
///
/// The same over CRLF and CR documents (before the fix, a comment
/// appended after a CR document took 187 µs at 3 KB and 17.9 ms at 300
/// KB, ratio 95: the document-tail window took only a `\n`-ended line
/// break as ending the inserted line).
#[test]
fn appending_after_a_final_block_scalar_stays_local() {
    let _serial = serial();
    for eol in EOLS {
        appending_after_a_final_block_scalar(eol);
    }
}

fn appending_after_a_final_block_scalar(eol: &str) {
    let name = eol_name(eol);
    let small = SyntaxTree::parse(ends_in_block_scalar(3 * 1024, eol));
    let large = SyntaxTree::parse(ends_in_block_scalar(300 * 1024, eol));
    let entry = Fragment::parse(&nl("k: x\n", eol))
        .unwrap()
        .unwrap_root(0)
        .unwrap();
    let comment = Fragment::parse(&nl("# end\n", eol)).unwrap();
    for (what, append) in [("a root key", true), ("a trailing comment", false)] {
        let setup = |t: &SyntaxTree| t.fork();
        let run = |t: &mut SyntaxTree| {
            if append {
                let map = t.root().first_child().unwrap().first_child().unwrap();
                let last = map.last_child().unwrap().id();
                let _ = t.insert_line_after(last, entry.clone()).unwrap();
            } else {
                let root = t.root().id();
                let _ = t.append_child(root, comment.clone()).unwrap();
            }
        };
        let (a, b) = alternate(7, || setup(&small), run, || setup(&large), run);
        let r = ratio(a, b);
        eprintln!("{name}: {what} after a block scalar: 3 KB: {a:?}, 300 KB: {b:?}, ratio {r:.2}");
        assert!(
            r < 4.0,
            "{name}: {what}: cost grew with the document: {r:.2}×"
        );
        for base in [&small, &large] {
            let mut t = setup(base);
            run(&mut t);
            let text = t.emit();
            let want = if append {
                format!("{}k: x{eol}", base.emit())
            } else {
                format!("{}# end{eol}", base.emit())
            };
            assert_eq!(text, want);
            assert_eq!(t, SyntaxTree::parse(text));
        }
    }
}

/// The rule's edges: the document-tail window is taken only
/// when it covers the inserted text and nothing past it can read it;
/// otherwise the document-level window decides, as before. Every edit is
/// accepted exactly when its text re-parses as the edited tree.
///
/// Documents with each line break, and inserted text with each
/// line break (mixed ones included: a `\r` inserted right before a `\n`
/// would read as one `\r\n`, the one hazard of accepting a `\r`-ended
/// line in the fast paths), before tails that start with a blank line.
#[test]
fn trivia_after_a_document_is_accepted_exactly_when_it_reads_so() {
    let (mut accepted, mut refused, mut floors) = (0, 0, 0);
    for doc_eol in EOLS {
        for ins_eol in EOLS {
            let (a, r) = trivia_after_a_document(doc_eol, ins_eol);
            let what = format!(
                "{} document, {} insertion",
                eol_name(doc_eol),
                eol_name(ins_eol)
            );
            eprintln!("{what}: {a} accepted, {r} refused");
            // A floor per combination, tuned to it
            // (measured: 435 accepted; 359 for a `\r` inserted into an LF
            // document and 305 for a `\n` into a CR one, where a `\r`
            // before a `\n` joins into one `\r\n` and more texts read
            // differently), so one combination losing its accepted cases
            // cannot hide behind the others.
            let floor = match (doc_eol, ins_eol) {
                ("\n", "\r") => 320,
                ("\r", "\n") => 270,
                _ => 390,
            };
            assert!(a > floor, "{what}: {a} accepted");
            floors += floor;
            accepted += a;
            refused += r;
        }
    }
    eprintln!("{accepted} accepted, {refused} refused");
    // The aggregate is the sum of the per-combination floors
    // (was `9 * 150`, which could not fail on its own).
    assert!(
        accepted > floors,
        "{accepted} accepted, floors sum {floors}"
    );
}

/// One combination of
/// [`trivia_after_a_document_is_accepted_exactly_when_it_reads_so`]: (accepted,
/// refused).
fn trivia_after_a_document(doc_eol: &str, ins_eol: &str) -> (usize, usize) {
    let bodies = [
        "a: >-\n  x\n",
        "a: |\n  x\n",
        "a: |+\n  x\n\n",
        "a:\n  - b: |\n      x\n",
        "- a\n- b\n",
        "a: 1\n# c\n",
        "--- |\nx\n",
        "a: b: c\n",
        "? a\n: >\n  x\n",
        "a: [x\n",
    ];
    let inserts = [
        "# end\n",
        "\n",
        "  # deeper\n",
        "  x\n",
        "\n\n# a\n# b\n",
        " ",
        "\t# t\n",
        "--- z\n",
    ];
    let lenient = Settings::new().lenient_inline_colons(true);
    let (mut accepted, mut refused) = (0, 0);
    let what = format!(
        "{} document, {} insertion",
        eol_name(doc_eol),
        eol_name(ins_eol)
    );
    for settings in [Settings::new(), lenient] {
        for body in bodies {
            let body = nl(body, doc_eol);
            let body = body.as_str();
            // At the end, and before a following document (the edit fuzz
            // found a space inserted before `---`, which makes it content:
            // the tail window does not read what follows, so the inserted
            // text must end its line), and before a blank line.
            for tail in ["", "--- y\n", "# t\n--- y\n", "\n--- y\n"] {
                let tail = nl(tail, doc_eol);
                let tail = tail.as_str();
                let src = format!("{body}{tail}");
                let tree = SyntaxTree::parse_with(src.as_str(), &settings);
                // The stream child the tail starts with.
                let before = tree
                    .root()
                    .children()
                    .find(|x| x.span().start == body.len() && !x.span().is_empty());
                for ins in inserts {
                    let ins = nl(ins, ins_eol);
                    let ins = ins.as_str();
                    let Ok(f) = Fragment::parse_with(ins, &settings) else {
                        continue;
                    };
                    let mut t = tree.fork();
                    let (r, expected) = match (tail, before) {
                        ("", _) | (_, None) => {
                            let root = t.root().id();
                            (t.append_child(root, f), format!("{src}{ins}"))
                        }
                        (_, Some(n)) => (t.insert_before(n.id(), f), format!("{body}{ins}{tail}")),
                    };
                    let again = SyntaxTree::parse_with(expected.as_str(), &settings);
                    match r {
                        Ok(_) => {
                            accepted += 1;
                            assert_eq!(t.emit(), expected, "{what}");
                            assert_eq!(t, again, "{what}: {src:?} + {ins:?}");
                        }
                        Err(e) => {
                            refused += 1;
                            assert_eq!(t.emit(), src, "{what}: {e}");
                            // Refused as not well-formed (or not insertable);
                            // Next to a document with errors in its
                            // content (`a: b: c` without the setting, `[x`),
                            // as read-only recovered input.
                            let recovered = tree.has_errors()
                                && e == TreeEditError::ReadOnly { kind: K::Error };
                            assert!(
                                recovered
                                    || matches!(
                                        e,
                                        TreeEditError::NotWellFormed
                                            | TreeEditError::Misplaced { .. }
                                    ),
                                "{what}: {src:?} + {ins:?}: {e:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    (accepted, refused)
}

// ===================================== a lenient warning leaves with its value

/// `lenient_inline_colons`: a value carrying a
/// `Y0028` warning could not be replaced at all (before the fix:
/// `replace_token("a: b" → "x")` and `replace(<Scalar>, "x")` refused as
/// not well-formed), though `k: x` re-parses cleanly. The warning is
/// attached to the line break after the value, its label inside the
/// value: it now leaves with the value, and the re-read decides.
#[test]
fn a_lenient_warning_leaves_with_its_value() {
    let settings = Settings::new().lenient_inline_colons(true);
    for src in [
        "k: a: b\nm: 1\n",
        "k: a: b # c\nm: 1\n",
        "k: a: b\n  c\nm: 1\n",
        "- k: a: b\n- m\n",
    ] {
        let tree = SyntaxTree::parse_with(src, &settings);
        assert!(!tree.has_errors());
        let (carrier, d) = tree.diagnostics_by_element().next().unwrap();
        assert_eq!(d.code, LENIENT_INLINE_COLON, "{src:?}");
        let carrier = carrier.unwrap().id();
        let value = tree
            .root()
            .descendants()
            .find(|x| x.kind() == K::PlainScalar && x.text().starts_with("a: b"))
            .unwrap();
        let handles: Vec<NodeId> = tree.root().descendants().map(|n| n.id()).collect();
        let check = |t: &SyntaxTree, what: &str| {
            assert_eq!(t.diagnostics().count(), 0, "{src:?} {what}");
            assert_eq!(*t, SyntaxTree::parse_with(t.emit(), &settings), "{what}");
            // No handle of an untouched element went stale: the
            // warning's element keeps its handle.
            assert!(t.get(carrier).is_ok());
            let live = handles.iter().filter(|h| t.get(**h).is_ok()).count();
            assert!(
                live + 2 >= handles.len(),
                "{what}: {live} of {}",
                handles.len()
            );
        };
        // The token.
        let mut t = tree.fork();
        let x = t.replace_token(value.id(), "x").unwrap();
        assert_eq!(t.get(x).unwrap().text(), "x");
        check(&t, "replace_token");
        // The node.
        let mut t = tree.fork();
        let node = value.parent().unwrap().id();
        let r = t.replace(node, Fragment::parse("'x'").unwrap()).unwrap();
        assert_eq!(r.removed.text(), value.text());
        check(&t, "replace");
        // The same text again keeps the warning where it was (the re-read
        // has it), on the same element.
        let mut t = tree.fork();
        let copy = t.extract(node, node).unwrap();
        let _ = t.replace(node, copy).unwrap();
        let (el, d) = t.diagnostics_by_element().next().unwrap();
        assert_eq!(d.code, LENIENT_INLINE_COLON);
        assert_eq!(el.unwrap().id(), carrier);
        assert_eq!(t, SyntaxTree::parse_with(t.emit(), &settings));
        // Refused twice (with the warning and without it: a block mapping
        // on the key's line reads as a lenient plain scalar again), the
        // edit leaves the warning where it was.
        let mut t = tree.fork();
        let before: Vec<String> = t.diagnostics().map(|d| format!("{d:?}")).collect();
        let map = Fragment::parse("p: q").unwrap();
        assert_eq!(map.root_kinds().next(), Some(K::BlockMapping));
        assert_eq!(t.replace(node, map), Err(TreeEditError::NotWellFormed));
        let after: Vec<String> = t.diagnostics().map(|d| format!("{d:?}")).collect();
        assert_eq!(before, after);
        assert_eq!(t.emit(), src);
        let (el, _) = t.diagnostics_by_element().next().unwrap();
        assert_eq!(el.unwrap().id(), carrier);
    }
    // A value replaced by text that is a lenient value again: the token
    // reader reads `c: d` alone as a mapping (it has no value context),
    // so the replacement is refused and the tree unchanged.
    let mut t = SyntaxTree::parse_with("k: a: b\nm: 1\n", &settings);
    let v = plain(&t, "a: b");
    assert!(t.replace_token(v, "c: d").is_err());
    assert_eq!(t.emit(), "k: a: b\nm: 1\n");
    assert_eq!(t.diagnostics().count(), 1);
}

/// A warning that left stays gone through later edits, forks,
/// copies, and the removal of its element; diagnostics read every way
/// agree with the re-parse.
#[test]
fn a_dropped_warning_stays_dropped() {
    let settings = Settings::new().lenient_inline_colons(true);
    let src = "k: a: b\nn: [1, 2]\nm: c: d\n";
    let mut t = SyntaxTree::parse_with(src, &settings);
    assert_eq!(t.diagnostics().count(), 2);
    let v = plain(&t, "a: b");
    let _ = t.replace_token(v, "x").unwrap();
    assert_eq!(t.diagnostics().count(), 1);
    // Later edits around it.
    let one = plain(&t, "1");
    let _ = t.replace_token(one, "11").unwrap();
    let f = t.fork();
    for tree in [&t, &f] {
        assert_eq!(*tree, SyntaxTree::parse_with(tree.emit(), &settings));
        // (The one left is attached to the text's last line break, a
        // stream child.)
        assert_eq!(tree.diagnostics_within(tree.root()).count(), 1);
        assert_eq!(tree.diagnostics_by_element().count(), 1);
        // The one left is the other value's, on its own element.
        let (el, d) = tree.diagnostics_by_element().next().unwrap();
        assert_eq!(el.unwrap().kind(), K::LineBreak);
        assert_eq!(&tree.emit()[d.primary.span.start..d.primary.span.end], ":");
        assert!(d.primary.span.start > tree.emit().find("m:").unwrap());
    }
    // The line break it was attached to, extracted and removed.
    let map = t.root().first_child().unwrap().first_child().unwrap();
    let brk = map
        .children()
        .find(|x| x.kind() == K::LineBreak)
        .unwrap()
        .id();
    let copy = t.extract(brk, brk).unwrap();
    assert_eq!(copy.diagnostics().count(), 0);
    // The other lenient value.
    let w = plain(&t, "c: d");
    let _ = t.replace_token(w, "y").unwrap();
    assert_eq!(t.diagnostics().count(), 0);
    assert_eq!(t.emit(), "k: x\nn: [11, 2]\nm: y\n");
    assert_eq!(t, SyntaxTree::parse_with(t.emit(), &settings));
}

/// The other two edits of the gha-workflow script: an entry
/// right after a value that ends with a block scalar starts its own line
/// (the scalar's text holds the line break), but was not seen as an entry
/// start, so an edit in it re-read the entry before it, a whole large
/// value (every replacement and removal of the appended key
/// re-read the workflow's `jobs`). Now its window starts at its own key.
/// Over LF, CRLF and CR documents.
#[test]
fn an_entry_after_a_block_scalar_value_is_edited_locally() {
    let _serial = serial();
    for eol in EOLS {
        an_entry_after_a_block_scalar_value(eol);
    }
}

fn an_entry_after_a_block_scalar_value(eol: &str) {
    let name = eol_name(eol);
    let make = |bytes: usize| {
        let mut s = String::from("jobs:\n");
        let mut i = 0;
        while s.len() < bytes {
            s.push_str(&format!("  job{i}:\n    steps: [a, b]\n"));
            i += 1;
        }
        s.push_str("  last:\n    run: >-\n      folded\n      text\nz: b\n");
        SyntaxTree::parse(nl(&s, eol))
    };
    let (small, large) = (make(3 * 1024), make(300 * 1024));
    // (`b` is also a flow item: the last `b` is `z`'s value.)
    let setup = |t: &SyntaxTree| {
        let f = t.fork();
        let id = f
            .root()
            .descendants()
            .filter(|x| x.kind() == K::PlainScalar && x.text() == "b")
            .last()
            .unwrap()
            .id();
        (f, id)
    };
    let run = |(t, id): &mut (SyntaxTree, NodeId)| flips(t, id);
    let (a, b) = alternate(5, || setup(&small), run, || setup(&large), run);
    let r = ratio(a, b);
    eprintln!(
        "{name}: 64 edits of an entry after a block scalar value: 3 KB: {a:?}, 300 KB: {b:?}, \
         ratio {r:.2}"
    );
    assert!(r < 4.0, "{name}: cost grew with the value before: {r:.2}×");
    let (mut t, mut id) = setup(&large);
    flips(&mut t, &mut id);
    assert!(t.emit().ends_with(&nl("      text\nz: b\n", eol)));
    assert_eq!(t, SyntaxTree::parse(t.emit()));
}
