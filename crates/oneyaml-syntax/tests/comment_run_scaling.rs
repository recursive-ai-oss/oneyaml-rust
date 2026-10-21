//! The comment-run index answers in
//! O(depth + log R + a line) **whatever the edit history**. Rewriting
//! every comment of a long footer (or its indentation, or inserting a line
//! before each) cuts the run into one overlay run per comment, each
//! starting on a line that began in the run before it; an index that kept
//! only what each run's own elements tell (a column "at least 0" for such
//! a comment) visited every one of those runs per question, so n rewrites
//! followed by n questions cost O(n²).
//!
//! Method (as `edit_scaling.rs`, robust against wall-clock noise): the
//! edits, and finding what to ask about, are not timed; a fixed number of
//! questions is, the minimum over
//! three fresh trees is kept, and the inputs differ 16× in size: a cost
//! linear in the run makes the large script about 16 times slower, the
//! bound allows 4× for cache and O(log) effects. Reported with
//! `--nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod work;

use std::fmt::Write as _;
use std::sync::Mutex;

use oneyaml_syntax::tree::{NodeId, SyntaxKind as K, SyntaxTree};

/// The timed tests run one at a time.
static TIMING: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An inner mapping whose footer has `n` comment lines, then an outer
/// entry (the footer's run ends at `z`).
fn footer_source(n: usize) -> String {
    let mut s = String::from("a:\n  b:\n    c: 1\n");
    for i in 0..n {
        writeln!(s, "    # footer line {i}").unwrap();
    }
    s.push_str("z: 1\n");
    s
}

/// How the run is cut before the questions.
#[derive(Clone, Copy, Debug)]
enum History {
    /// Every comment rewritten (`replace_token`).
    RewriteComments,
    /// Every comment's indentation rewritten with itself.
    RewriteIndentation,
    /// A comment line inserted before every comment.
    InsertBefore,
}

fn edited(n: usize, history: History) -> SyntaxTree {
    let mut t = SyntaxTree::parse(footer_source(n));
    let comments: Vec<NodeId> = t
        .root()
        .descendants()
        .filter(|x| x.kind() == K::Comment)
        .map(|x| x.id())
        .collect();
    assert_eq!(comments.len(), n);
    let line = SyntaxTree::parse("# inserted\n    ");
    let kids: Vec<_> = line.root().children().collect();
    let frag = line
        .extract(kids[0].id(), kids[kids.len() - 1].id())
        .unwrap();
    for (i, c) in comments.iter().enumerate() {
        match history {
            History::RewriteComments => {
                t.replace_token(*c, &format!("# rewritten {i}")).unwrap();
            }
            History::RewriteIndentation => {
                let ws = t.get(*c).unwrap().prev_sibling().unwrap();
                assert_eq!(ws.kind(), K::Whitespace);
                let ws = ws.id();
                t.replace_token(ws, "    ").unwrap();
            }
            History::InsertBefore => {
                t.insert_before(*c, frag.clone()).unwrap();
            }
        }
    }
    t
}

/// The steps of `QUESTIONS` index questions on `t` (finding them is not
/// counted): the footer's last comment (rule 13's cut above 0), and the
/// owner of comments at the run's start, middle and end.
fn questions(t: &SyntaxTree) -> u64 {
    const QUESTIONS: usize = 300;
    let inner = t
        .root()
        .descendants()
        .filter(|x| x.kind() == K::BlockMapping)
        .nth(2)
        .unwrap();
    let comments: Vec<_> = t
        .root()
        .descendants()
        .filter(|x| x.kind() == K::Comment)
        .collect();
    let picks = [
        comments[0],
        comments[comments.len() / 2],
        comments[comments.len() - 1],
    ];
    work::reset();
    let mut sink = 0;
    for k in 0..QUESTIONS {
        sink += usize::from(t.last_footer_comment(inner).is_some());
        let a = t.comment_attachment(picks[k % 3]).unwrap();
        sink += a.blank_lines_before();
    }
    std::hint::black_box(sink);
    work::taken()
}

fn per_question(n: usize, history: History) -> u64 {
    let mut min = u64::MAX;
    for _ in 0..3 {
        let t = edited(n, history);
        min = min.min(questions(&t));
    }
    min
}

#[test]
#[ignore = "slow: runs in CI"]
fn index_questions_do_not_scale_with_the_edit_history() {
    let _g = serial();
    for history in [
        History::RewriteComments,
        History::RewriteIndentation,
        History::InsertBefore,
    ] {
        let small = per_question(1_000, history);
        let large = per_question(16_000, history);
        let r = work::ratio("questions", small, large);
        eprintln!("{history:?}: {small:?} at 1k, {large:?} at 16k (ratio {r:.2})");
        assert!(
            r < 4.0,
            "{history:?}: index questions grow with the edit history: ratio {r:.2}"
        );
    }
    // The answers are the pass's (the edited footer is still the inner
    // mapping's, its last line the footer's last comment).
    let t = edited(300, History::RewriteComments);
    let all = t.comment_attachments();
    for (c, a) in &all {
        assert_eq!(t.comment_attachment(*c).as_ref(), Some(a));
    }
    let inner = t
        .root()
        .descendants()
        .filter(|x| x.kind() == K::BlockMapping)
        .nth(2)
        .unwrap();
    assert_eq!(
        t.last_footer_comment(inner).map(|c| c.text().into_owned()),
        Some("# rewritten 299".to_owned())
    );
}
