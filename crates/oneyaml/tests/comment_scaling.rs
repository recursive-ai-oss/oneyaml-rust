//! The comment API's cost does not grow with the comment
//! run it works in. `push_footer` onto a footer of 500 and of 8,000
//! comment lines costs the same per call (it used to read the whole run:
//! O(footer) per call, O(n²) for n pushes), and re-owning every comment
//! of a stretch of comment lines broken by stray `...` markers costs the
//! same per comment for 1,000 and 16,000 of them (it used to walk the
//! stretch for each: O(n²)).
//!
//! Method (as `document_scaling.rs`): the tests count the steps the syntax
//! tree takes instead of measuring time, and only ratios are asserted. Each
//! script runs three times on freshly parsed documents (parsing is not
//! counted) and the fewest steps are kept; the inputs differ 16× in size,
//! so a cost linear in the run would make the large script take about 16
//! times more steps per call, while the bound allows 4×. Reported with
//! `--nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::fmt::Write as _;

use oneyaml::{CommentOwner, Document, Stream};

mod work;

/// A nested mapping whose inner footer already has `n` comment lines,
/// followed by a document footer.
fn footer_source(n: usize) -> String {
    let mut s = String::from("top:\n  list:\n    - a\n    - b\n");
    for i in 0..n {
        writeln!(s, "    # footer line {i}").unwrap();
    }
    s.push_str("  next: 1\n# document footer\n");
    s
}

/// The fewest steps over three runs of `script` on a fresh document.
fn best(src: &str, script: &dyn Fn(&mut Document)) -> u64 {
    let mut min = u64::MAX;
    for _ in 0..3 {
        let mut doc = Document::parse(src).unwrap();
        work::reset();
        script(&mut doc);
        min = min.min(work::taken());
    }
    min
}

/// `a` and `b` are steps per call on the small and the large input.
fn ratio(name: &str, a: f64, b: f64) {
    let r = b / a.max(1.0);
    eprintln!("{name}: {a:.1} small, {b:.1} large (ratio {r:.2})");
    assert!(r < 4.0, "{name} grows with the comment run: ratio {r:.2}");
}

#[test]
fn push_footer_does_not_scale_with_the_footer() {
    const PUSHES: usize = 100;
    let script = |doc: &mut Document| {
        let list = doc.get("top").unwrap().get("list").unwrap().id();
        for i in 0..PUSHES {
            doc.push_footer(list, &format!("pushed {i}")).unwrap();
        }
    };
    let small = footer_source(500);
    let large = footer_source(8_000);
    ratio(
        "100 push_footer calls",
        best(&small, &script) as f64,
        best(&large, &script) as f64,
    );
    // The lines went where the footer ends, in order, and are its footer.
    let mut doc = Document::parse(footer_source(30)).unwrap();
    script(&mut doc);
    let list = doc.get("top").unwrap().get("list").unwrap().id();
    let footer = doc.comments(list).unwrap();
    let texts: Vec<&str> = footer.footer().iter().map(|c| c.text()).collect();
    assert_eq!(texts.len(), 30 + PUSHES);
    assert_eq!(texts[29], "footer line 29");
    assert_eq!(texts[30], "pushed 0");
    assert_eq!(texts[30 + PUSHES - 1], format!("pushed {}", PUSHES - 1));
    let text = doc.to_string();
    assert!(text.ends_with("    # pushed 99\n  next: 1\n# document footer\n"));
    assert!(Document::parse(text.as_str()).unwrap().to_string() == text);
}

/// A document, then `n` comment lines with a stray `...` every eighth
/// line (rule 7: all of them its document footer).
fn stretch_source(n: usize) -> String {
    let mut s = String::from("a: 1\n...\n");
    for i in 0..n {
        writeln!(s, "# stretch {i}").unwrap();
        if i % 8 == 7 {
            s.push_str("...\n");
        }
    }
    s
}

#[test]
fn bulk_attach_comment_is_linear() {
    let run = |n: usize| {
        let src = stretch_source(n);
        let mut min = u64::MAX;
        for _ in 0..3 {
            let mut stream = Stream::parse(src.as_str()).unwrap();
            let doc = stream.document(0).unwrap();
            let ids = doc.comment_ids();
            assert_eq!(ids.len(), n);
            let root = doc.root().unwrap().id();
            work::reset();
            let mut d = stream.document_mut(0).unwrap();
            for c in &ids {
                d.attach_comment(*c, CommentOwner::Leading(root)).unwrap();
            }
            for c in &ids {
                assert!(d.detach_comment(*c).unwrap().is_some());
            }
            min = min.min(work::taken());
        }
        // Per comment.
        min as f64 / n as f64
    };
    ratio(
        "attach_comment + detach_comment per comment",
        run(1_000),
        run(16_000),
    );
    // A stretch read as one run (no stray `...`), with a document after it
    // whose header it is.
    let run = |n: usize| {
        let mut src = String::from("a: 1\n...\n");
        for i in 0..n {
            writeln!(src, "# header {i}").unwrap();
        }
        src.push_str("b: 2\n");
        let mut min = u64::MAX;
        for _ in 0..3 {
            let mut doc = Stream::parse(src.as_str()).unwrap().document(1).unwrap();
            let ids = doc.comment_ids();
            assert_eq!(ids.len(), n);
            work::reset();
            for c in &ids {
                doc.attach_comment(*c, CommentOwner::DocumentFooter)
                    .unwrap();
            }
            min = min.min(work::taken());
        }
        min as f64 / n as f64
    };
    ratio("attach_comment per header comment", run(1_000), run(16_000));
}
