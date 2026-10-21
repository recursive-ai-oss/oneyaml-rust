//! The comment API — reading comments by owner
//! (`Document::comments`, `NodeRef::comments`, `header`, `footer`), the
//! writes (`set_leading`, `set_trailing`, `push_footer`, `clear_comments`,
//! `set_header`, `set_footer`), explicit re-owning (`attach_comment`,
//! `detach_comment`), view vs storage, every error arm, streams,
//! restricted documents, and the round-trip property: after comment
//! edits the text, parsed again, explains its comments exactly as the
//! edited document does, and untouched bytes stay.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod work;

use oneyaml::{
    CommentExplanation, CommentOwner, CommentRule, Document, EditError, NodeId, Settings, Stream,
    path,
};

const SRC: &str = "\
# header

# lead a
a: 1 # trailing a
b:
  - x
  # footer of b
c: 3 # trailing c
# document footer
";

/// What a comment explanation says, without handles: comparable across
/// documents (the owner by its span).
fn shape(e: &CommentExplanation) -> (usize, String, String, Option<(usize, usize)>, u8) {
    let owner = match e.owner() {
        CommentOwner::DocumentHeader => "header",
        CommentOwner::DocumentFooter => "footer",
        CommentOwner::Leading(_) => "leading",
        CommentOwner::Trailing(_) => "trailing",
        CommentOwner::Footer(_) => "collection footer",
        _ => "other",
    };
    (
        e.span().start,
        e.text().to_owned(),
        owner.to_owned(),
        e.owner_span().map(|s| (s.start, s.end)),
        e.rule().number(),
    )
}

/// Both readers agree, the round-trip property: the edited document explains
/// its comments as a parse of its text does, and that text re-emits
/// byte for byte.
fn reparses_the_same(d: &Document) {
    let text = d.to_string();
    let again = Document::parse_with(text.clone(), d.settings()).unwrap();
    assert_eq!(again.to_string().as_bytes(), text.as_bytes());
    let mine: Vec<_> = d.explain_comments().unwrap().iter().map(shape).collect();
    let theirs: Vec<_> = again
        .explain_comments()
        .unwrap()
        .iter()
        .map(shape)
        .collect();
    assert_eq!(mine, theirs, "text: {text:?}");
    // The meaning is untouched by comment edits.
    assert_eq!(d.to_value().unwrap(), again.to_value().unwrap());
}

fn key(d: &Document, p: &oneyaml::Path) -> NodeId {
    d.get_path(p).unwrap().key().unwrap().id()
}

fn texts(c: &[oneyaml::Comment]) -> Vec<&str> {
    c.iter().map(oneyaml::Comment::text).collect()
}

/// The rule of a `CommentReattaches` refusal.
fn reattach<T: std::fmt::Debug>(r: Result<T, EditError>) -> Option<CommentRule> {
    match r {
        Err(EditError::CommentReattaches { rule, .. }) => Some(rule),
        other => panic!("expected CommentReattaches, got {other:?}"),
    }
}

/// The text of an `InvalidComment` refusal.
fn invalid<T: std::fmt::Debug>(r: Result<T, EditError>) -> Option<String> {
    match r {
        Err(EditError::InvalidComment { text, .. }) => Some(text),
        other => panic!("expected InvalidComment, got {other:?}"),
    }
}

// ================================================================ reads

#[test]
fn reads_comments_by_owner() {
    let d = Document::parse(SRC).unwrap();
    let a = key(&d, &path!["a"]);
    let c = d.comments(a).unwrap();
    assert_eq!(texts(c.leading()), ["lead a"]);
    assert_eq!(texts(c.trailing()), ["trailing a"]);
    assert!(c.footer().is_empty());
    assert_eq!(c.leading()[0].blank_lines_before(), 1);
    assert_eq!(c.leading()[0].raw(), "# lead a");
    assert!(!c.leading()[0].is_explicit());
    assert_eq!(
        &SRC[c.leading()[0].span().start..c.leading()[0].span().end],
        "# lead a"
    );
    assert!(!c.is_empty());
    // The value names none of its entry's comments.
    let v = d.get("a").unwrap();
    assert!(v.comments().unwrap().is_empty());
    // A collection's footer.
    let b = d.get("b").unwrap();
    assert_eq!(texts(b.comments().unwrap().footer()), ["footer of b"]);
    // Header and footer.
    assert_eq!(texts(&d.header().unwrap()), ["header"]);
    assert_eq!(texts(&d.footer().unwrap()), ["document footer"]);
    // The handles are the explanation's.
    let e = d.explain_comment(c.trailing()[0].id()).unwrap();
    assert_eq!(e.owner(), CommentOwner::Trailing(a));
    // Every comment is read by exactly one owner (header, footer, nodes).
    let mut read = d.header().unwrap().len() + d.footer().unwrap().len();
    let mut stack = vec![d.root().unwrap()];
    while let Some(n) = stack.pop() {
        read += n.comments().unwrap().leading().len();
        read += n.comments().unwrap().trailing().len();
        read += n.comments().unwrap().footer().len();
        if let Some(m) = n.as_mapping() {
            for (k, v) in m.iter() {
                let kc = k.comments().unwrap();
                read += kc.leading().len() + kc.trailing().len();
                stack.push(v);
            }
        }
        if let Some(s) = n.as_sequence() {
            stack.extend(s.iter());
        }
    }
    assert_eq!(read, d.comment_ids().len());
}

#[test]
fn key_names_the_entry() {
    let d = Document::parse("a: 1\nl: [x]\n").unwrap();
    let v = d.get("a").unwrap();
    assert_eq!(v.key().unwrap().text(), "a");
    // Not a mapping value: no key.
    assert!(d.root().unwrap().key().is_none());
    assert!(v.key().unwrap().key().is_none());
    assert!(d.get_path(&path!["l", 0]).unwrap().key().is_none());
}

#[test]
fn view_strips_one_space_storage_is_exact() {
    let src = "a: 1 #note\nb: 2 #  two\nc: 3 #\ttab\nd: 4 #\ne: 5 # trailing  \n";
    let d = Document::parse(src).unwrap();
    let t = |k: &str| {
        let c = d.comments(key(&d, &path![k])).unwrap();
        let x = &c.trailing()[0];
        (x.text().to_owned(), x.raw().to_owned())
    };
    assert_eq!(t("a"), ("note".into(), "#note".into()));
    assert_eq!(t("b"), (" two".into(), "#  two".into()));
    assert_eq!(t("c"), ("\ttab".into(), "#\ttab".into()));
    assert_eq!(t("d"), (String::new(), "#".into()));
    assert_eq!(t("e"), ("trailing  ".into(), "# trailing  ".into()));
    assert_eq!(d.to_string().as_bytes(), src.as_bytes());
}

#[test]
fn blank_lines_before_a_node() {
    let d = Document::parse("a: 1\n\n\nb: 2\n\n# c\nc: 3\nd: |+\n  x\n\ne: 4\n").unwrap();
    let blanks = |k: &str| d.comments(key(&d, &path![k])).unwrap().blank_lines_before();
    assert_eq!(blanks("a"), 0);
    assert_eq!(blanks("b"), 2);
    // Below its leading comment.
    assert_eq!(blanks("c"), 0);
    // `|+` keeps its blank line: content, not separation (rule 8).
    assert_eq!(blanks("e"), 0);
    // A value on its key's line does not start a line.
    assert_eq!(
        d.get("a").unwrap().comments().unwrap().blank_lines_before(),
        0
    );
}

#[test]
fn flow_comments_are_read_item_by_item() {
    let src = "k: [ # open\n  # lead x\n  x, # after x\n  y\n  # end\n  ] # close\n";
    let d = Document::parse(src).unwrap();
    let k = key(&d, &path!["k"]);
    let kc = d.comments(k).unwrap();
    assert_eq!(texts(kc.trailing()), ["open", "close"]);
    let x = d.get_path(&path!["k", 0]).unwrap().comments().unwrap();
    assert_eq!(texts(x.leading()), ["lead x"]);
    assert_eq!(texts(x.trailing()), ["after x"]);
    let seq = d.get("k").unwrap().comments().unwrap();
    assert_eq!(texts(seq.footer()), ["end"]);
}

#[test]
fn reading_a_stale_handle_is_an_error() {
    let mut d = Document::parse("a: 1\nb: 2\n").unwrap();
    let a = d.get("a").unwrap().id();
    let _ = d.remove_path(&path!["a"]).unwrap();
    assert_eq!(d.comments(a), Err(EditError::StaleNode));
    let other = Document::parse("x: 1\n").unwrap();
    let foreign = other.root().unwrap().id();
    assert_eq!(d.comments(foreign), Err(EditError::StaleNode));
}

#[test]
fn input_without_documents_has_only_a_header() {
    let mut d = Document::parse("# one\n# two\n").unwrap();
    assert!(d.root().is_none());
    assert_eq!(texts(&d.header().unwrap()), ["one", "two"]);
    assert!(d.footer().unwrap().is_empty());
    assert_eq!(
        reattach(d.set_footer(&["x"])),
        Some(CommentRule::HeaderBlankLine)
    );
    d.set_header(&["new"]).unwrap();
    assert_eq!(d.to_string(), "# new\n");
    let mut e = Document::parse("").unwrap();
    e.set_header(&["h"]).unwrap();
    assert_eq!(e.to_string(), "# h\n");
    let mut n = Document::parse("# no break").unwrap();
    n.set_header(&["no break", "and more"]).unwrap();
    assert_eq!(n.to_string(), "# no break\n# and more\n");
    // `set_header` alone keeps an input without documents one.
    assert_eq!(texts(&n.header().unwrap()), ["no break", "and more"]);
}

// =============================================================== writes

#[test]
fn set_leading_replaces_in_place() {
    let mut d = Document::parse(SRC).unwrap();
    let a = key(&d, &path!["a"]);
    let new = d.set_leading(a, &["first", "", " indented"]).unwrap();
    assert_eq!(new.len(), 3);
    assert_eq!(
        d.to_string(),
        SRC.replace("# lead a\n", "# first\n#\n#  indented\n")
    );
    let c = d.comments(a).unwrap();
    assert_eq!(texts(c.leading()), ["first", "", " indented"]);
    assert_eq!(c.leading().iter().map(|x| x.id()).collect::<Vec<_>>(), new);
    reparses_the_same(&d);
    // Removed: the blank line above stays.
    assert!(d.set_leading(a, oneyaml::NO_COMMENTS).unwrap().is_empty());
    assert_eq!(d.to_string(), SRC.replace("# lead a\n", ""));
    assert!(d.comments(a).unwrap().leading().is_empty());
    reparses_the_same(&d);
    // Nothing to do.
    let before = d.to_string();
    assert!(d.set_leading(a, oneyaml::NO_COMMENTS).unwrap().is_empty());
    assert_eq!(d.to_string(), before);
}

#[test]
fn set_leading_in_nested_collections_and_line_breaks() {
    let src = "a:\n  b: 1\n  c:\n    - x\n    - y\nd: |\n  text\ne: 1\n";
    for nl in ["\n", "\r\n", "\r"] {
        let src = src.replace('\n', nl);
        let mut d = Document::parse(src.clone()).unwrap();
        let b = key(&d, &path!["a", "b"]);
        d.set_leading(b, &["about b"]).unwrap();
        let y = d.get_path(&path!["a", "c", 1]).unwrap().id();
        d.set_leading(y, &["about y"]).unwrap();
        let e = key(&d, &path!["e"]);
        d.set_leading(e, &["after a block scalar"]).unwrap();
        let want = "a:\n  # about b\n  b: 1\n  c:\n    - x\n    # about y\n    - y\nd: |\n  text\n# after a block scalar\ne: 1\n";
        assert_eq!(d.to_string(), want.replace('\n', nl), "{nl:?}");
        assert_eq!(texts(d.comments(y).unwrap().leading()), ["about y"]);
        reparses_the_same(&d);
    }
}

#[test]
fn set_leading_of_a_value_on_its_own_line() {
    // C12: `k:⏎  # c⏎  v` leads the value.
    let mut d = Document::parse("k:\n  v\n").unwrap();
    let v = d.get("k").unwrap().id();
    d.set_leading(v, &["the value"]).unwrap();
    assert_eq!(d.to_string(), "k:\n  # the value\n  v\n");
    reparses_the_same(&d);
}

#[test]
fn set_leading_errors() {
    let mut d = Document::parse(SRC).unwrap();
    let before = d.to_string();
    let a = key(&d, &path!["a"]);
    // NEL, LS and PS too (they are line breaks to some readers).
    for bad in [
        "two\nlines",
        "cr\rhere",
        "bom\u{feff}",
        "bell\u{7}",
        "nel\u{85}here",
        "ls\u{2028}here",
        "ps\u{2029}here",
    ] {
        assert_eq!(
            invalid(d.set_leading(a, &["fine", bad])).as_deref(),
            Some(bad),
            "{bad:?}"
        );
    }
    // A value on its key's line does not start a line.
    let v = d.get("a").unwrap().id();
    assert!(matches!(
        d.set_leading(v, &["x"]),
        Err(EditError::Unsupported { .. })
    ));
    // Above a root mapping, rule 3 gives the line to its first entry.
    let root = d.root().unwrap().id();
    assert_eq!(
        reattach(d.set_leading(root, &["x"])),
        Some(CommentRule::FirstNodeLeading)
    );
    // Stale.
    let mut other = d.clone();
    let _ = other.remove_path(&path!["a"]).unwrap();
    assert_eq!(other.set_leading(a, &["x"]), Err(EditError::StaleNode));
    assert_eq!(d.to_string(), before, "refused writes change nothing");
    // Display names the error.
    let e = d.set_leading(root, &["x"]).unwrap_err();
    assert!(e.to_string().contains("rule 3"), "{e}");
    assert!(e.at().is_none());
    let e = d.set_leading(a, &["a\nb"]).unwrap_err();
    assert!(e.to_string().contains("cannot be a comment"), "{e}");
    assert!(e.at().is_none());
}

#[test]
fn set_trailing_writes_replaces_and_removes() {
    let mut d = Document::parse(SRC).unwrap();
    let a = key(&d, &path!["a"]);
    let b = key(&d, &path!["b"]);
    // Replaced in place: the white space before it stays.
    let id = d.set_trailing(a, Some("new a")).unwrap().unwrap();
    assert!(d.to_string().contains("a: 1 # new a\n"));
    assert_eq!(d.comments(a).unwrap().trailing()[0].id(), id);
    // New, on a key line whose value is a block collection.
    d.set_trailing(b, Some("the list")).unwrap();
    assert!(d.to_string().contains("b: # the list\n  - x\n"));
    // On an item.
    let x = d.get_path(&path!["b", 0]).unwrap().id();
    d.set_trailing(x, Some("item")).unwrap();
    assert!(d.to_string().contains("  - x # item\n"));
    reparses_the_same(&d);
    // Removed.
    assert_eq!(d.set_trailing(a, None).unwrap(), None);
    assert!(d.to_string().contains("a: 1\nb:"));
    assert_eq!(d.set_trailing(a, None).unwrap(), None);
    reparses_the_same(&d);
    // The last entry at the end of a text without a final line break.
    let mut e = Document::parse("a: 1\nz: 2").unwrap();
    let z = key(&e, &path!["z"]);
    e.set_trailing(z, Some("end")).unwrap();
    assert_eq!(e.to_string(), "a: 1\nz: 2 # end");
    reparses_the_same(&e);
    // A flow value spanning lines: after its closing bracket.
    let mut f = Document::parse("k: [1,\n  2]\nz: 1\n").unwrap();
    let k = key(&f, &path!["k"]);
    f.set_trailing(k, Some("k")).unwrap();
    assert_eq!(f.to_string(), "k: [1,\n  2] # k\nz: 1\n");
    reparses_the_same(&f);
}

#[test]
fn set_trailing_errors() {
    let mut d = Document::parse("a: 1\nl:\n  - k: v\ns: |\n  x\n").unwrap();
    let before = d.to_string();
    let v = d.get("a").unwrap().id();
    assert!(matches!(
        d.set_trailing(v, Some("x")),
        Err(EditError::Unsupported { .. })
    ));
    // `- k: v`: the line's comment would trail `k`, not the item.
    let item = d.get_path(&path!["l", 0]).unwrap().id();
    assert_eq!(
        reattach(d.set_trailing(item, Some("x"))),
        Some(CommentRule::Trailing)
    );
    // A block scalar's header line (0.3 limitation).
    let s = key(&d, &path!["s"]);
    assert!(matches!(
        d.set_trailing(s, Some("x")),
        Err(EditError::Unsupported { .. })
    ));
    let a = key(&d, &path!["a"]);
    assert_eq!(
        invalid(d.set_trailing(a, Some("\n"))).as_deref(),
        Some("\n")
    );
    assert_eq!(d.to_string(), before);
    // An existing comment of a block scalar's header can be replaced.
    let mut e = Document::parse("s: | # old\n  x\n").unwrap();
    let s = key(&e, &path!["s"]);
    e.set_trailing(s, Some("new")).unwrap();
    assert_eq!(e.to_string(), "s: | # new\n  x\n");
    reparses_the_same(&e);
}

#[test]
fn push_footer_block_and_flow() {
    let mut d = Document::parse(SRC).unwrap();
    let b = d.get("b").unwrap().id();
    d.push_footer(b, "- y").unwrap();
    d.push_footer(b, "- z").unwrap();
    assert!(
        d.to_string()
            .contains("  - x\n  # footer of b\n  # - y\n  # - z\nc: 3")
    );
    assert_eq!(
        texts(d.comments(b).unwrap().footer()),
        ["footer of b", "- y", "- z"]
    );
    reparses_the_same(&d);
    // After the footers of collections inside it (rule 13).
    let mut n = Document::parse("a:\n  b:\n    - 1\n    # inner\nc: 1\n").unwrap();
    let inner = n.get("a").unwrap().id();
    n.push_footer(inner, "outer").unwrap();
    assert_eq!(
        n.to_string(),
        "a:\n  b:\n    - 1\n    # inner\n  # outer\nc: 1\n"
    );
    reparses_the_same(&n);
    // After a block scalar ending the collection, and at the end of a text
    // without a final line break.
    let mut s = Document::parse("o:\n  k: |\n    t\nz: 1").unwrap();
    let o = s.get("o").unwrap().id();
    s.push_footer(o, "f").unwrap();
    assert_eq!(s.to_string(), "o:\n  k: |\n    t\n  # f\nz: 1");
    reparses_the_same(&s);
    let mut e = Document::parse("o:\n  - 1\nz:\n  - 2").unwrap();
    let z = e.get("z").unwrap().id();
    e.push_footer(z, "f").unwrap();
    assert_eq!(e.to_string(), "o:\n  - 1\nz:\n  - 2\n  # f");
    reparses_the_same(&e);
    // Flow: above a closing bracket on its own line.
    let mut f = Document::parse("k: [\n  a,\n  ]\n").unwrap();
    let k = f.get("k").unwrap().id();
    f.push_footer(k, "more").unwrap();
    assert_eq!(f.to_string(), "k: [\n  a,\n  # more\n  ]\n");
    assert_eq!(texts(f.comments(k).unwrap().footer()), ["more"]);
    reparses_the_same(&f);
}

#[test]
fn push_footer_errors() {
    let mut d = Document::parse("a: 1\nk: [x]\np: [q: 1]\nl:\n  - 1\n").unwrap();
    let before = d.to_string();
    let v = d.get("a").unwrap().id();
    assert!(matches!(
        d.push_footer(v, "x"),
        Err(EditError::Unsupported { .. })
    ));
    let k = d.get("k").unwrap().id();
    assert!(matches!(
        d.push_footer(k, "x"),
        Err(EditError::Unsupported { .. })
    ));
    let pair = d.get_path(&path!["p", 0]).unwrap().id();
    assert!(matches!(
        d.push_footer(pair, "x"),
        Err(EditError::Unsupported { .. })
    ));
    // A root collection at column 0: a line after it is the document
    // footer (rule 7).
    let root = d.root().unwrap().id();
    assert_eq!(
        reattach(d.push_footer(root, "x")),
        Some(CommentRule::DocumentFooter)
    );
    let l = d.get("l").unwrap().id();
    assert_eq!(invalid(d.push_footer(l, "a\nb")).as_deref(), Some("a\nb"));
    assert_eq!(d.to_string(), before);
}

#[test]
fn clear_comments_hands_back_what_it_removes() {
    let mut d = Document::parse(SRC).unwrap();
    let total = d.comment_ids().len();
    let a = key(&d, &path!["a"]);
    let gone = d.clear_comments(a).unwrap();
    assert_eq!(gone, ["# lead a", "# trailing a"]);
    let b = d.get("b").unwrap().id();
    assert_eq!(d.clear_comments(b).unwrap(), ["# footer of b"]);
    assert!(d.clear_comments(b).unwrap().is_empty());
    assert_eq!(d.comment_ids().len(), total - 3);
    assert_eq!(
        d.to_string(),
        "# header\n\na: 1\nb:\n  - x\nc: 3 # trailing c\n# document footer\n"
    );
    reparses_the_same(&d);
}

#[test]
fn header_and_footer_setters() {
    let mut d = Document::parse(SRC).unwrap();
    d.set_header(&["new header"]).unwrap();
    assert!(d.to_string().starts_with("# new header\n\n# lead a\n"));
    // Idempotent: no blank line piles up.
    d.set_header(&["new header"]).unwrap();
    assert!(d.to_string().starts_with("# new header\n\n# lead a\n"));
    assert_eq!(texts(&d.header().unwrap()), ["new header"]);
    d.set_footer(&["f1", "f2"]).unwrap();
    assert!(d.to_string().ends_with("c: 3 # trailing c\n# f1\n# f2\n"));
    assert_eq!(texts(&d.footer().unwrap()), ["f1", "f2"]);
    reparses_the_same(&d);
    // Removed: the blank line after the header stays.
    d.set_header(oneyaml::NO_COMMENTS).unwrap();
    d.set_footer(oneyaml::NO_COMMENTS).unwrap();
    assert!(d.header().unwrap().is_empty() && d.footer().unwrap().is_empty());
    assert!(d.to_string().starts_with("\n# lead a\n"));
    assert!(d.set_header(oneyaml::NO_COMMENTS).unwrap().is_empty());
    assert!(d.set_footer(oneyaml::NO_COMMENTS).unwrap().is_empty());
    reparses_the_same(&d);
    // A header where the first node had only leading comments.
    let mut e = Document::parse("# lead\na: 1\n").unwrap();
    e.set_header(&["h"]).unwrap();
    assert_eq!(e.to_string(), "# h\n\n# lead\na: 1\n");
    let a = key(&e, &path!["a"]);
    assert_eq!(texts(e.comments(a).unwrap().leading()), ["lead"]);
    reparses_the_same(&e);
    // With `---`: before it (rule 1), no blank line needed.
    let mut s = Document::parse("%YAML 1.2\n---\na: 1\n...\n").unwrap();
    s.set_header(&["h"]).unwrap();
    s.set_footer(&["f"]).unwrap();
    assert_eq!(s.to_string(), "# h\n%YAML 1.2\n---\na: 1\n# f\n...\n");
    reparses_the_same(&s);
    // Footer at the end of a text without a final line break.
    let mut n = Document::parse("a: 1").unwrap();
    n.set_footer(&["f"]).unwrap();
    assert_eq!(n.to_string(), "a: 1\n# f");
    reparses_the_same(&n);
    // No root: a footer cannot be written (rule 2 makes it header).
    let mut r = Document::parse("--- # just a marker\n").unwrap();
    assert!(r.root().is_some_and(|n| n.span().is_empty()));
    assert_eq!(texts(&r.header().unwrap()), ["just a marker"]);
    assert_eq!(
        reattach(r.set_footer(&["f"])),
        Some(CommentRule::HeaderBlankLine)
    );
    assert_eq!(
        invalid(r.set_header(&["a", "b\u{0}"])).as_deref(),
        Some("b\u{0}")
    );
    r.set_header(&["h"]).unwrap();
    assert_eq!(r.to_string(), "# h\n---\n");
    reparses_the_same(&r);
}

#[test]
fn stream_documents_write_their_own_header_and_footer() {
    let src = "a: 1\n---\nb:\n  - 2\n...\n# after\n--- c\n";
    let mut s = Stream::parse(src).unwrap();
    {
        let mut d = s.document_mut(0).unwrap();
        // Before the next document's `---` a line is its header (rule 1).
        assert_eq!(
            reattach(d.set_footer(&["f0"])),
            Some(CommentRule::HeaderBeforeStart)
        );
        d.set_header(&["h0"]).unwrap();
    }
    {
        let mut d = s.document_mut(1).unwrap();
        d.set_header(&["h1"]).unwrap();
        d.set_footer(&["f1"]).unwrap();
        let b = d.get("b").unwrap().id();
        d.node_mut(b).unwrap().push_footer("pf").unwrap();
        let bk = d.get("b").unwrap().key().unwrap().id();
        d.set_leading(bk, &["lb"]).unwrap();
        d.set_trailing(bk, Some("tb")).unwrap();
        let x = d.clear_comments(bk).unwrap();
        assert_eq!(x, ["# lb", "# tb"]);
    }
    {
        let mut d = s.document_mut(2).unwrap();
        assert_eq!(texts(&d.header().unwrap()), ["after"]);
        d.set_header(&["h2"]).unwrap();
        d.set_footer(&["f2"]).unwrap();
    }
    assert_eq!(
        s.to_string(),
        "# h0\n\na: 1\n# h1\n---\nb:\n  - 2\n  # pf\n# f1\n...\n# h2\n--- c\n# f2\n"
    );
    let again = Stream::parse(s.to_string()).unwrap();
    for i in 0..3 {
        let mine: Vec<_> = s
            .document(i)
            .unwrap()
            .explain_comments()
            .unwrap()
            .iter()
            .map(shape)
            .collect();
        let theirs: Vec<_> = again
            .document(i)
            .unwrap()
            .explain_comments()
            .unwrap()
            .iter()
            .map(shape)
            .collect();
        assert_eq!(mine, theirs, "document {i}");
    }
}

#[test]
fn restricted_and_read_only_documents() {
    let p = Document::parse_recovering("a: 1\nb: [1\nc: 3\n");
    let mut d = p.into_value();
    let before = d.to_string();
    // Next to the error region.
    let b = d.get("b").unwrap().id();
    assert!(matches!(
        d.push_footer(b, "x"),
        Err(EditError::ReadOnly { .. })
    ));
    assert_eq!(d.to_string(), before);
    // An error region itself owns no comments.
    let mut r = Document::parse_recovering("a: 1\nb: 'x\n").into_value();
    let e = r.errors().next().expect("an error region").id();
    let before = r.to_string();
    assert!(matches!(
        r.set_leading(e, &["x"]),
        Err(EditError::ReadOnly { .. })
    ));
    assert!(matches!(
        r.clear_comments(e),
        Err(EditError::ReadOnly { .. })
    ));
    assert_eq!(r.to_string(), before);
    // Encoding errors: nothing is editable.
    let mut bytes = b"a: 1\nb: \xff\n".to_vec();
    bytes.extend_from_slice(b"c: 2\n");
    let mut r = Document::from_slice_recovering(&bytes, &Settings::new()).into_value();
    let rk = key(&r, &path!["a"]);
    assert!(matches!(
        r.set_leading(rk, &["x"]),
        Err(EditError::ReadOnly { .. })
    ));
}

// ============================================================ re-owning

#[test]
fn explicit_owners_beat_the_rulebook_in_this_document_only() {
    let mut d = Document::parse(SRC).unwrap();
    let footer_b = d.get("b").unwrap().comments().unwrap().footer()[0].id();
    let c = key(&d, &path!["c"]);
    // `# footer of b` is really about `c`.
    d.attach_comment(footer_b, CommentOwner::Leading(c))
        .unwrap();
    let e = d.explain_comment(footer_b).unwrap();
    assert_eq!(e.owner(), CommentOwner::Leading(c));
    assert_eq!(e.rule(), CommentRule::Explicit);
    assert_eq!(e.rule().number(), 0);
    assert!(e.to_string().contains("rule 0 (explicit"), "{e}");
    let cc = d.comments(c).unwrap();
    assert_eq!(texts(cc.leading()), ["footer of b"]);
    assert!(cc.leading()[0].is_explicit());
    assert!(d.get("b").unwrap().comments().unwrap().footer().is_empty());
    assert_eq!(
        d.explain_comments()
            .unwrap()
            .iter()
            .filter(|x| x.rule() == CommentRule::Explicit)
            .count(),
        1
    );
    // The bytes do not change; a parse of them uses the rulebook (the
    // documented exception).
    assert_eq!(d.to_string(), SRC);
    let again = Document::parse(d.to_string()).unwrap();
    assert_eq!(
        again
            .explain_comment(again.comment_ids()[3])
            .unwrap()
            .rule(),
        CommentRule::CollectionFooter
    );
    // Clones keep it.
    let copy = d.clone();
    assert_eq!(
        copy.explain_comment(footer_b).unwrap().rule(),
        CommentRule::Explicit
    );
    // Writes treat it as the explicit owner's: set_leading(c) replaces it.
    let mut w = d.clone();
    w.set_leading(c, &["about c"]).unwrap();
    assert!(!w.to_string().contains("footer of b"));
    assert!(w.to_string().contains("  - x\n# about c\nc: 3"));
    reparses_the_same(&w);
    // Detach: the rulebook decides again.
    assert_eq!(
        d.detach_comment(footer_b).unwrap(),
        Some(CommentOwner::Leading(c))
    );
    assert_eq!(d.detach_comment(footer_b).unwrap(), None);
    assert_eq!(
        d.explain_comment(footer_b).unwrap().rule(),
        CommentRule::CollectionFooter
    );
    // Header and footer owners.
    let lead = d.comments(key(&d, &path!["a"])).unwrap().leading()[0].id();
    d.attach_comment(lead, CommentOwner::DocumentHeader)
        .unwrap();
    assert_eq!(texts(&d.header().unwrap()), ["header", "lead a"]);
    let tc = d.comments(c).unwrap().trailing()[0].id();
    d.attach_comment(tc, CommentOwner::DocumentFooter).unwrap();
    assert_eq!(
        texts(&d.footer().unwrap()),
        ["trailing c", "document footer"]
    );
    let b = d.get("b").unwrap().id();
    d.attach_comment(tc, CommentOwner::Footer(b)).unwrap();
    assert_eq!(
        texts(d.comments(b).unwrap().footer()),
        ["footer of b", "trailing c"]
    );
    let a = key(&d, &path!["a"]);
    d.attach_comment(tc, CommentOwner::Trailing(a)).unwrap();
    assert_eq!(
        texts(d.comments(a).unwrap().trailing()),
        ["trailing a", "trailing c"]
    );
}

#[test]
fn explicit_owners_lapse_with_their_node_and_their_comment() {
    let mut d = Document::parse("a: 1 # ta\nb: 2\nc: 3\n").unwrap();
    let ta = d.comment_ids()[0];
    let b = key(&d, &path!["b"]);
    d.attach_comment(ta, CommentOwner::Leading(b)).unwrap();
    let _ = d.remove_path(&path!["b"]).unwrap();
    // The owner is gone: the rulebook decides.
    assert_eq!(d.explain_comment(ta).unwrap().rule(), CommentRule::Trailing);
    assert_eq!(d.detach_comment(ta).unwrap(), None);
    // Its comment removed: the explicit owner goes with it.
    let c = key(&d, &path!["c"]);
    d.attach_comment(ta, CommentOwner::Leading(c)).unwrap();
    let got = d.clear_comments(c).unwrap();
    assert_eq!(got, ["# ta"]);
    assert_eq!(d.explain_comment(ta), Err(EditError::StaleNode));
    assert_eq!(d.to_string(), "a: 1\nc: 3\n");
}

#[test]
fn attach_and_detach_errors() {
    let mut d = Document::parse("# h\na: [x] # c\n").unwrap();
    let c = d.comment_ids()[1];
    let a = key(&d, &path!["a"]);
    let v = d.get("a").unwrap().id();
    let x = d.get_path(&path!["a", 0]).unwrap().id();
    assert!(matches!(
        d.attach_comment(c, CommentOwner::Footer(x)),
        Err(EditError::Unsupported { .. })
    ));
    d.attach_comment(c, CommentOwner::Footer(v)).unwrap();
    // Stale comment, foreign owner.
    let other = Document::parse("# o\nz: 1\n").unwrap();
    let foreign_comment = other.comment_ids()[0];
    assert_eq!(
        d.attach_comment(foreign_comment, CommentOwner::Leading(a)),
        Err(EditError::StaleNode)
    );
    assert_eq!(d.detach_comment(foreign_comment), Err(EditError::StaleNode));
    let foreign_node = other.root().unwrap().id();
    assert_eq!(
        d.attach_comment(c, CommentOwner::Leading(foreign_node)),
        Err(EditError::StaleNode)
    );
    // An input without documents has no document footer.
    let mut e = Document::parse("# only\n").unwrap();
    let only = e.comment_ids()[0];
    assert!(matches!(
        e.attach_comment(only, CommentOwner::DocumentFooter),
        Err(EditError::Unsupported { .. })
    ));
    e.attach_comment(only, CommentOwner::DocumentHeader)
        .unwrap();
    // An error region owns no comments.
    let mut r = Document::parse_recovering("# c\na: 'x\n").into_value();
    let err = r.errors().next().unwrap().id();
    let rc = r.comment_ids()[0];
    assert!(matches!(
        r.attach_comment(rc, CommentOwner::Leading(err)),
        Err(EditError::Unsupported { .. })
    ));
}

#[test]
fn stream_keeps_explicit_owners_across_documents() {
    let mut s = Stream::parse("a: 1 # c\n---\nb: 2\n").unwrap();
    let c;
    {
        let mut d = s.document_mut(0).unwrap();
        c = d.comment_ids()[0];
        let a = d.get("a").unwrap().key().unwrap().id();
        d.attach_comment(c, CommentOwner::Leading(a)).unwrap();
    }
    {
        let d = s.document_mut(1).unwrap();
        assert_eq!(d.explain_comment(c), Err(EditError::StaleNode));
    }
    let snap = s.document(0).unwrap();
    assert_eq!(
        snap.explain_comment(c).unwrap().rule(),
        CommentRule::Explicit
    );
    let mut d = s.document_mut(0).unwrap();
    assert!(d.detach_comment(c).unwrap().is_some());
}

// ======================================================= no recursion, scaling

#[test]
fn deep_nesting_on_a_small_stack() {
    // Reads and writes are loops.
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let depth = 2_000;
            let mut src = String::new();
            for i in 0..depth {
                src.push_str(&" ".repeat(i));
                src.push_str("- # c\n");
            }
            src.push_str(&" ".repeat(depth));
            src.push_str("x\n");
            let settings = Settings::new().limits(oneyaml::Limits::unlimited());
            let mut d = Document::parse_with(src, &settings).unwrap();
            let mut n = d.root().unwrap();
            while let Some(s) = n.as_sequence() {
                n = s.get(0).unwrap();
            }
            let id = n.id();
            assert!(d.comments(id).unwrap().leading().is_empty());
            d.set_leading(id, &["deep"]).unwrap();
            assert_eq!(texts(d.comments(id).unwrap().leading()), ["deep"]);
            assert_eq!(d.comment_ids().len(), depth + 1);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn comment_edits_are_local() {
    // The same edit on a 50× larger document costs about the same.
    fn time(entries: usize) -> u64 {
        let src: String = (0..entries).map(|i| format!("k{i}: v # t{i}\n")).collect();
        let mut d = Document::parse(src).unwrap();
        let mid = key(&d, &path![format!("k{}", entries / 2)]);
        work::reset();
        for i in 0..20 {
            d.set_leading(mid, &[&format!("l{i}")]).unwrap();
            d.set_trailing(mid, Some(&format!("t{i}"))).unwrap();
            let _ = d.comments(mid).unwrap();
        }
        work::taken()
    }
    let small = (0..3).map(|_| time(200)).min().unwrap();
    let large = (0..3).map(|_| time(10_000)).min().unwrap();
    assert!(large < small * 10, "small {small:?}, large {large:?}");
}

// ====================================================== cost fixes

/// Runs `op` on `d`: refused, nothing changed; accepted, the meaning is
/// the one before and a parse of the text agrees.
fn meaning_kept<T: std::fmt::Debug>(
    d: &mut Document,
    op: impl FnOnce(&mut Document) -> Result<T, EditError>,
) -> Result<T, EditError> {
    let (text, value) = (d.to_string(), d.to_value().unwrap());
    let r = op(d);
    match &r {
        Err(e) => {
            assert!(!matches!(e, EditError::Internal), "{e:?}");
            assert_eq!(d.to_string(), text, "refused, changed: {e:?}");
        }
        Ok(_) => {
            assert_eq!(
                d.to_value().unwrap(),
                value,
                "meaning changed: {:?}",
                d.to_string()
            );
            reparses_the_same(d);
        }
    }
    r
}

#[test]
fn keep_scalars_before_a_trailing_spaces_line_keep_their_value() {
    // Known repros: a comment line added or
    // removed between a `|+` scalar and a last line of spaces without a
    // line break would change the scalar's value.
    let mut d = Document::parse("k: |+\n  text\n  ").unwrap();
    assert_eq!(d.to_value().unwrap()["k"].as_str(), Some("text\n\n"));
    let r = meaning_kept(&mut d, |d| d.set_footer(&["x"]));
    assert!(matches!(r, Err(EditError::Unsupported { .. })), "{r:?}");

    let mut d = Document::parse("- |+\n    # c1\n# c2\n# c3\n   ").unwrap();
    let _ = meaning_kept(&mut d, |d| d.set_footer(oneyaml::NO_COMMENTS));

    let mut d = Document::parse("k: |+\n  text\n# f\n  ").unwrap();
    let c = d.comment_ids()[0];
    let k = key(&d, &path!["k"]);
    d.attach_comment(c, CommentOwner::Leading(k)).unwrap();
    let r = meaning_kept(&mut d, |d| d.clear_comments(k));
    assert!(matches!(r, Err(EditError::Unsupported { .. })), "{r:?}");
    assert_eq!(d.to_value().unwrap()["k"].as_str(), Some("text\n"));

    // The same class through every write.
    for src in [
        "k: |+\n  text\n# c\n  ",
        "k: |+\n  text\n\n# c\n  ",
        "a: 1\nk: |+\n  text\n# lead m\nm: |+\n  x\n  ",
        "k: |+ # t\n  text\n  ",
        "- |+\n  x\n# f\n  ",
        "k: |+\n  text\n  \n",
    ] {
        for step in 0..8 {
            let mut d = Document::parse(src).unwrap();
            let root = d.root().unwrap().id();
            let mut nodes = vec![root];
            if let Some(m) = d.root().unwrap().as_mapping() {
                for (k, v) in m.iter() {
                    nodes.push(k.id());
                    nodes.push(v.id());
                }
            }
            if let Some(s) = d.root().unwrap().as_sequence() {
                nodes.extend(s.iter().map(|n| n.id()));
            }
            for n in nodes {
                let mut e = d.clone();
                let _ = match step {
                    0 => meaning_kept(&mut e, |e| e.set_leading(n, &["x"]).map(|_| ())),
                    1 => meaning_kept(&mut e, |e| {
                        e.set_leading(n, oneyaml::NO_COMMENTS).map(|_| ())
                    }),
                    2 => meaning_kept(&mut e, |e| e.set_trailing(n, None).map(|_| ())),
                    3 => meaning_kept(&mut e, |e| e.set_trailing(n, Some("t")).map(|_| ())),
                    4 => meaning_kept(&mut e, |e| e.push_footer(n, "p").map(|_| ())),
                    5 => meaning_kept(&mut e, |e| e.clear_comments(n).map(|_| ())),
                    6 => meaning_kept(&mut e, |e| e.set_footer(oneyaml::NO_COMMENTS).map(|_| ())),
                    _ => meaning_kept(&mut e, |e| e.set_header(&["h"]).map(|_| ())),
                };
            }
            let _ = meaning_kept(&mut d, |d| d.set_footer(&["f"]));
        }
    }
}

/// The fewest steps over three runs of `f`.
fn best(mut f: impl FnMut() -> u64) -> u64 {
    (0..3).map(|_| f()).min().unwrap()
}

#[test]
fn comment_run_length_scaling() {
    // The k comments of one owner are read,
    // replaced, and removed in O(k) (one splice for consecutive comment
    // lines), where removing them one splice at a time was O(k²) (45 s for
    // 16 000 lines).
    fn time(k: usize) -> u64 {
        let mut src = String::from("a: 1\n");
        for i in 0..k {
            src.push_str(&format!("# c{i}\n"));
        }
        src.push_str("b: 2\n");
        let mut d = Document::parse(src).unwrap();
        let b = key(&d, &path!["b"]);
        let lines: Vec<String> = (0..k).map(|i| format!("l{i}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        work::reset();
        assert_eq!(d.comments(b).unwrap().leading().len(), k);
        assert_eq!(d.clear_comments(b).unwrap().len(), k);
        assert_eq!(d.set_leading(b, &lines).unwrap().len(), k);
        assert_eq!(d.set_leading(b, oneyaml::NO_COMMENTS).unwrap().len(), 0);
        let t = work::taken();
        assert_eq!(d.to_string(), "a: 1\nb: 2\n");
        t
    }
    let small = best(|| time(1_000));
    let large = best(|| time(16_000));
    assert!(large < small * 40, "1k: {small:?}, 16k: {large:?}");
}

#[test]
fn explicit_owner_count_does_not_slow_reads_writes_or_snapshots() {
    // Explicit owners are indexed by
    // owner (reads and writes look up their own, O(log k)) and shared by
    // clones and stream snapshots (O(1)). Bulk re-owning inside a clean
    // document costs O(depth) per comment.
    // The same document (8000 comment lines) with k of them re-owned.
    fn build(k: usize) -> (Stream, u64) {
        let mut src = String::from("---\n");
        for i in 0..8_000 {
            src.push_str(&format!("# e{i}\n"));
        }
        src.push_str("x: 1\nm:\n  - 1\nz: 2\n");
        let mut s = Stream::parse(src).unwrap();
        let attach = {
            let mut d = s.document_mut(0).unwrap();
            let ids = d.comment_ids();
            assert_eq!(ids.len(), 8_000);
            work::reset();
            for c in ids.into_iter().take(k) {
                d.attach_comment(c, CommentOwner::DocumentHeader).unwrap();
            }
            work::taken()
        };
        (s, attach)
    }
    fn work(s: &mut Stream) -> u64 {
        work::reset();
        for i in 0..20 {
            // Snapshots and clones are O(1) (dropped before the edits: an
            // edit of a shared tree copies it, as documented).
            let z = {
                let snap = s.document(0).unwrap();
                let copy = snap.clone();
                let z = key(&copy, &path!["z"]);
                let _ = copy.comments(z).unwrap();
                z
            };
            let mut d = s.document_mut(0).unwrap();
            d.set_trailing(z, Some(&format!("t{i}"))).unwrap();
            d.set_leading(z, &["lead"]).unwrap();
            let m = d.get("m").unwrap().id();
            d.node_mut(m).unwrap().push_footer("f").unwrap();
            let _ = d.clear_comments(m).unwrap();
        }
        work::taken()
    }
    let (mut s_small, attach_small) = build(100);
    let (mut s_large, attach_large) = build(8_000);
    let small = best(|| work(&mut s_small));
    let large = best(|| work(&mut s_large));
    assert!(
        large < small * 4,
        "100 explicit: {small:?}, 8000: {large:?}"
    );
    // Linear, not quadratic, in the comments re-owned (80×).
    assert!(
        attach_large < attach_small * 400,
        "attach 100: {attach_small:?}, 8000: {attach_large:?}"
    );
    // The explicit owners still read back.
    assert_eq!(s_large.document(0).unwrap().header().unwrap().len(), 8_000);
    assert_eq!(s_small.document(0).unwrap().header().unwrap().len(), 100);
}

#[test]
fn push_footer_cost_follows_its_run() {
    // A push costs at most O(the run) (it read the run after the collection for
    // its place and to check it, rule 13, and the syntax layer checks an
    // insertion into a run of comment lines over that run), never more. With
    // the comment-run index a block collection's push costs O(depth + log) with
    // the comment-run index (`tests/comment_scaling.rs` pins that); this bound
    // still holds.
    fn time(k: usize) -> u64 {
        let mut src = String::from("l:\n  - a\n");
        for i in 0..k {
            src.push_str(&format!("  # f{i}\n"));
        }
        src.push_str("z: 1\n");
        let mut d = Document::parse(src).unwrap();
        let l = d.get("l").unwrap().id();
        work::reset();
        for i in 0..10 {
            d.push_footer(l, &format!("p{i}")).unwrap();
        }
        let t = work::taken();
        assert_eq!(d.comments(l).unwrap().footer().len(), k + 10);
        t
    }
    let small = best(|| time(500));
    let large = best(|| time(4_000));
    assert!(large < small * 20, "500: {small:?}, 4000: {large:?}");
}

#[test]
fn stream_documents_through_document_mut_keep_meaning() {
    // A kept scalar at the end of
    // the last document.
    let mut s = Stream::parse("a: 1\n---\nk: |+\n  text\n  ").unwrap();
    let before = s.to_string();
    let mut d = s.document_mut(1).unwrap();
    let r = d.set_footer(&["x"]);
    assert!(matches!(r, Err(EditError::Unsupported { .. })), "{r:?}");
    assert_eq!(s.to_string(), before);
    assert_eq!(
        s.document(1).unwrap().to_value().unwrap()["k"].as_str(),
        Some("text\n\n")
    );
}

// ====================================================== explicit owners

/// This replaces a limitation test ("explicit owners lapse
/// when a node edit rewrites their comment"): explicit owners follow their
/// comment through node edits that carry or rewrite it.
#[test]
fn explicit_owners_follow_their_comment_through_node_edits() {
    // A move carries `# x` (it leads `b`, rule 4) with its explicit owner.
    let mut d = Document::parse("a: 1\n# x\nb: 2\nc: 3\n").unwrap();
    let x = d.comment_ids()[0];
    let c = key(&d, &path!["c"]);
    d.attach_comment(x, CommentOwner::Leading(c)).unwrap();
    d.root_mut().unwrap().move_entry(1, 2).unwrap();
    assert_eq!(d.to_string(), "a: 1\nc: 3\n# x\nb: 2\n");
    let e = d.explain_comments().unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].rule(), CommentRule::Explicit);
    assert_eq!(e[0].owner(), CommentOwner::Leading(c));
    assert_eq!(d.comments(c).unwrap().leading()[0].raw(), "# x");

    // A same-line comment moved onto a new block scalar's header (the new
    // text replaces a collection) keeps its explicit owner.
    let mut d = Document::parse("a: [1] # t\nb: 2\n").unwrap();
    let t = d.comment_ids()[0];
    let b = key(&d, &path!["b"]);
    d.attach_comment(t, CommentOwner::Leading(b)).unwrap();
    d.set_path(&path!["a"], "x\ny\n").unwrap();
    assert!(
        d.to_string().starts_with("a: | # t\n"),
        "{:?}",
        d.to_string()
    );
    let e = d.explain_comments().unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].rule(), CommentRule::Explicit);
    assert_eq!(d.comments(b).unwrap().leading().len(), 1);

    // Removal and re-insertion: the explicit owner is recorded relative to
    // the entry (here, a node inside it) and resolved on the new entry.
    let mut d = Document::parse("a: 1\n# x\nb:\n  k: 2\nc: 3\n").unwrap();
    let x = d.comment_ids()[0];
    let k = key(&d, &path!["b", "k"]);
    d.attach_comment(x, CommentOwner::Trailing(k)).unwrap();
    let r = d.remove_path(&path!["b"]).unwrap();
    assert_eq!(r.comments(), ["# x"]);
    assert_eq!(r.comment_ids(), [x]);
    assert!(d.explain_comments().unwrap().is_empty());
    d.root_mut().unwrap().insert_removed_at(0, r).unwrap();
    assert_eq!(d.to_string(), "# x\nb:\n  k: 2\na: 1\nc: 3\n");
    let k2 = key(&d, &path!["b", "k"]);
    let e = d.explain_comments().unwrap();
    assert_eq!(e[0].rule(), CommentRule::Explicit);
    assert_eq!(e[0].owner(), CommentOwner::Trailing(k2));

    // An explicit owner inside a moved entry, of a comment that stays:
    // it follows the entry.
    let mut d = Document::parse("a: 1 # t\nb: 2\nc: 3\n").unwrap();
    let t = d.comment_ids()[0];
    let b = key(&d, &path!["b"]);
    d.attach_comment(t, CommentOwner::Leading(b)).unwrap();
    d.root_mut().unwrap().move_entry(1, 2).unwrap();
    let b2 = key(&d, &path!["b"]);
    assert_ne!(b, b2);
    let e = d.explain_comments().unwrap();
    assert_eq!(e[0].rule(), CommentRule::Explicit);
    assert_eq!(e[0].owner(), CommentOwner::Leading(b2));

    // It lapses only with the comment, or with an owner that does not
    // come back.
    let mut d = Document::parse("a: 1 # t\nb: 2\nc: 3\n").unwrap();
    let t = d.comment_ids()[0];
    let b = key(&d, &path!["b"]);
    d.attach_comment(t, CommentOwner::Leading(b)).unwrap();
    let _ = d.remove_path(&path!["b"]).unwrap();
    let e = d.explain_comments().unwrap();
    assert_eq!(e[0].rule(), CommentRule::Trailing);
}

#[test]
fn a_line_taken_by_another_owners_comment_is_reported_as_such() {
    let mut d = Document::parse("a: 1\nb: 2 # t\n").unwrap();
    let t = d.comment_ids()[0];
    let a = key(&d, &path!["a"]);
    let b = key(&d, &path!["b"]);
    d.attach_comment(t, CommentOwner::Trailing(a)).unwrap();
    let before = d.to_string();
    match d.set_trailing(b, Some("n")) {
        Err(EditError::Unsupported { reason, .. }) => {
            assert!(reason.contains("already ends with a comment"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(d.to_string(), before);
    // Released, the line takes the new comment.
    d.detach_comment(t).unwrap();
    d.set_trailing(b, Some("n")).unwrap();
    assert_eq!(d.to_string(), "a: 1\nb: 2 # n\n");
}

#[test]
fn identity_fuzz_findings_are_read_by_their_owner() {
    // Found by the identity accounting of the fuzz. A comment
    // between an item's `-` and its node on a later line leads the item.
    let mut d = Document::parse("-\n # c1\n  \"flow in block\"\n- x\n").unwrap();
    let item = d.get_path(&path![0]).unwrap().id();
    assert_eq!(texts(d.comments(item).unwrap().leading()), ["c1"]);
    assert_eq!(d.clear_comments(item).unwrap(), ["# c1"]);
    assert!(d.comment_ids().is_empty());
    reparses_the_same(&d);
    // A comment before a stray `...` between documents is the next
    // document's header (rule 5, C16).
    let mut s = Stream::parse("Bare\ndocument\n...\n# No document\n...\nv\n").unwrap();
    assert_eq!(
        texts(&s.document(1).unwrap().header().unwrap()),
        ["No document"]
    );
    let mut d = s.document_mut(1).unwrap();
    assert_eq!(d.set_header(oneyaml::NO_COMMENTS).unwrap(), []);
    assert_eq!(s.to_string(), "Bare\ndocument\n...\n...\nv\n");
}
