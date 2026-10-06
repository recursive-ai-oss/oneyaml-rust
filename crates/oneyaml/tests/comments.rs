//! The public comment-attachment API of `Document` —
//! `comment_ids`, `explain_comment`, `explain_comments` — its handles
//! across edits and clones, stream documents, error paths,
//! and recovered input.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use oneyaml::{CommentOwner, CommentRule, Document, EditError, NodeKind, Stream, path};

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

fn rules(d: &Document) -> Vec<(String, u8)> {
    d.explain_comments()
        .unwrap()
        .iter()
        .map(|e| (e.text().to_owned(), e.rule().number()))
        .collect()
}

#[test]
fn explains_every_comment_in_source_order() {
    let d = Document::parse(SRC).unwrap();
    assert_eq!(
        rules(&d),
        [
            ("# header".to_owned(), 2),
            ("# lead a".to_owned(), 3),
            ("# trailing a".to_owned(), 5),
            ("# footer of b".to_owned(), 6),
            ("# trailing c".to_owned(), 5),
            ("# document footer".to_owned(), 7),
        ]
    );
    let ids = d.comment_ids();
    let all = d.explain_comments().unwrap();
    assert_eq!(ids.len(), all.len());
    for (id, e) in ids.iter().zip(&all) {
        assert_eq!(e.id(), *id);
        assert_eq!(&d.explain_comment(*id).unwrap(), e);
        // Byte-exact text at its span.
        assert_eq!(&SRC[e.span().start..e.span().end], e.text());
    }
    // Owners resolve to the nodes the rules name.
    let a_key = d.get_path(&path!["a"]).unwrap();
    let lead = &all[1];
    let CommentOwner::Leading(id) = lead.owner() else {
        panic!("{lead:?}")
    };
    let owner = d.node(id).unwrap();
    assert_eq!(owner.text(), "a");
    assert!(
        owner.span().end <= a_key.span().start,
        "the key names the entry"
    );
    assert_eq!(lead.owner_span(), Some(owner.span()));
    assert_eq!(lead.blank_lines_before(), 1);
    let CommentOwner::Footer(b) = all[3].owner() else {
        panic!("{:?}", all[3])
    };
    assert_eq!(b, d.id_of(&path!["b"]).unwrap());
    assert_eq!(all[0].owner(), CommentOwner::DocumentHeader);
    assert_eq!(all[0].owner_span(), None);
    assert_eq!(all[5].owner(), CommentOwner::DocumentFooter);
}

#[test]
fn display_names_the_rule() {
    let d = Document::parse(SRC).unwrap();
    let e = &d.explain_comments().unwrap()[3];
    let s = e.to_string();
    assert!(s.contains("rule 6"), "{s}");
    assert!(s.contains(CommentRule::CollectionFooter.summary()), "{s}");
    assert!(s.contains("footer of the collection at"), "{s}");
    let h = d.explain_comments().unwrap()[0].to_string();
    assert!(h.ends_with("document header"), "{h}");
}

#[test]
fn rule_numbers_are_the_rulebook_numbers() {
    let all = [
        (CommentRule::HeaderBeforeStart, 1),
        (CommentRule::HeaderBlankLine, 2),
        (CommentRule::FirstNodeLeading, 3),
        (CommentRule::Leading, 4),
        (CommentRule::Trailing, 5),
        (CommentRule::CollectionFooter, 6),
        (CommentRule::DocumentFooter, 7),
        (CommentRule::Flow, 9),
        (CommentRule::ShallowLeading, 10),
        (CommentRule::ExplicitValue, 11),
        (CommentRule::Inside, 12),
        (CommentRule::RunOrder, 13),
    ];
    for (r, n) in all {
        assert_eq!(r.number(), n);
        assert!(!r.summary().is_empty());
    }
    // Every numbered rule is in the rulebook, and rule 8 (blank lines) too.
    // Inside the workspace the rulebook must be there.
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/comment-attachment.md");
    match std::fs::read_to_string(&path) {
        Ok(book) => {
            for n in 1..=13 {
                assert!(book.contains(&format!("### Rule {n} ")), "rule {n}");
            }
        }
        Err(e) => {
            assert!(!in_workspace(), "{}: {e}", path.display());
            eprintln!("skipped: the rulebook is not in the published package");
        }
    }
}

/// Tested inside the oneyaml workspace (not from the unpacked package).
fn in_workspace() -> bool {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    std::fs::read_to_string(root)
        .is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""))
}

#[test]
fn handles_survive_edits_elsewhere_and_go_stale_with_their_comment() {
    let mut d = Document::parse(SRC).unwrap();
    let before = d.explain_comments().unwrap();
    // An edit elsewhere: every handle still resolves, to the same rule.
    d.set_path(&path!["a"], 2).unwrap();
    for e in &before {
        let now = d.explain_comment(e.id()).unwrap();
        assert_eq!(now.rule(), e.rule(), "{e}");
        assert_eq!(now.text(), e.text());
    }
    // Removing `c` takes its trailing comment (comments in a removed
    // range travel with `Removed`): that handle is stale, typed.
    let trailing_c = before[4].id();
    let removed = d.get_mut_root_mapping_remove("c");
    assert!(removed);
    assert_eq!(d.explain_comment(trailing_c), Err(EditError::StaleNode));
    assert!(!d.comment_ids().contains(&trailing_c));
    // Comment rule 6: `# footer of
    // b` is the footer of `b`'s sequence, not `c`'s, and stays with its
    // owner. With `c` gone the parser places it after the root mapping
    // (C4), so it is written there again: same text and owner, a new
    // handle (the old one is stale). The other comments keep their
    // handles, and all are explained on the edited tree.
    let footer_b = &before[3];
    assert_eq!(footer_b.text(), "# footer of b");
    let left = d.comment_ids();
    let fresh: Vec<_> = left
        .iter()
        .filter(|id| !before.iter().any(|e| e.id() == **id))
        .collect();
    assert_eq!(fresh.len(), 1, "{fresh:?}");
    let now = d.explain_comment(*fresh[0]).unwrap();
    assert_eq!(now.text(), footer_b.text());
    assert_eq!(now.owner(), footer_b.owner());
    assert_eq!(now.rule(), CommentRule::CollectionFooter);
    assert_eq!(d.explain_comment(footer_b.id()), Err(EditError::StaleNode));
    assert_eq!(left.len(), before.len() - 1);
    for id in left {
        d.explain_comment(id).unwrap();
    }
    let footer = before[5].id();
    assert_eq!(
        d.explain_comment(footer).unwrap().rule(),
        CommentRule::DocumentFooter
    );
}

/// `root_mut().remove(key)`, discarding the removed entry.
trait RemoveEntry {
    fn get_mut_root_mapping_remove(&mut self, key: &str) -> bool;
}

impl RemoveEntry for Document {
    fn get_mut_root_mapping_remove(&mut self, key: &str) -> bool {
        let removed = self.root_mut().unwrap().remove(key).unwrap();
        drop(removed);
        true
    }
}

#[test]
fn clones_share_handles_and_other_documents_do_not() {
    let d = Document::parse(SRC).unwrap();
    let ids = d.comment_ids();
    let mut copy = d.clone();
    copy.set_path(&path!["a"], 5).unwrap();
    for id in &ids {
        assert_eq!(
            copy.explain_comment(*id).unwrap().rule(),
            d.explain_comment(*id).unwrap().rule()
        );
    }
    let other = Document::parse(SRC).unwrap();
    assert_eq!(other.explain_comment(ids[0]), Err(EditError::StaleNode));
}

#[test]
fn stream_documents_own_their_comments() {
    let src = "a: 1\n# f0\n# before one\n---\nb: 2 # t1\n...\n# after end\n--- # s2\nc\n# f2\n";
    let s = Stream::parse(src).unwrap();
    let docs: Vec<Document> = s.documents().collect();
    let texts = |d: &Document| rules(d).into_iter().map(|(t, _)| t).collect::<Vec<_>>();
    // `# f0` and `# before one` are at root column before `---`: the next
    // document's header (rule 1).
    assert_eq!(texts(&docs[0]), Vec::<String>::new());
    assert_eq!(texts(&docs[1]), ["# f0", "# before one", "# t1"]);
    assert_eq!(texts(&docs[2]), ["# after end", "# s2", "# f2"]);
    // A comment of document 1, asked of document 0: stale (not its own).
    let t1 = docs[1].comment_ids()[2];
    assert_eq!(docs[0].explain_comment(t1), Err(EditError::StaleNode));
    assert_eq!(
        docs[1].explain_comment(t1).unwrap().rule(),
        CommentRule::Trailing
    );
    // Every comment belongs to exactly one document.
    let total: usize = docs.iter().map(|d| d.comment_ids().len()).sum();
    assert_eq!(total, src.matches('#').count());
}

#[test]
fn a_node_handle_is_not_a_comment() {
    // CommentId has no public constructor; a stale handle is the only way
    // to name nothing, covered above. An empty document has no comments.
    let d = Document::parse("").unwrap();
    assert!(d.comment_ids().is_empty());
    assert!(d.explain_comments().unwrap().is_empty());
    let only = Document::parse("# just a comment\n").unwrap();
    let e = only.explain_comments().unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].owner(), CommentOwner::DocumentHeader);
    assert_eq!(e[0].rule(), CommentRule::HeaderBlankLine);
}

#[test]
fn recovered_documents_explain_every_comment() {
    for src in [
        "a: [1, 2 # open\n# c\nb: 1\n",
        "- a\n# c\n  - b # t\n",
        "k: 'unclosed # x\n# y\n",
        "{a: 1 # t\n# c\n",
        "a: 1\n  b: 2 # bad indent\n# c\n",
    ] {
        let d = Document::parse_recovering(src).into_value();
        let all = d.explain_comments().unwrap();
        for e in &all {
            assert_eq!(&d.explain_comment(e.id()).unwrap(), e, "{src:?}");
            if let CommentOwner::Leading(n) | CommentOwner::Trailing(n) | CommentOwner::Footer(n) =
                e.owner()
            {
                // Owners are nodes of this document (error regions included).
                assert!(d.node(n).is_ok(), "{src:?}: {e}");
            }
        }
    }
}

#[test]
fn stream_edits_keep_other_documents_comments() {
    let mut s = Stream::parse("a: 1 # t0\n---\nb: 2 # t1\n").unwrap();
    let t1 = s.document(1).unwrap().comment_ids()[0];
    s.document_mut(0)
        .unwrap()
        .set_path(&path!["a"], 10)
        .unwrap();
    let d1 = s.document(1).unwrap();
    assert_eq!(d1.explain_comment(t1).unwrap().text(), "# t1");
}

/// A recovering `Document` over several documents
/// explains what it navigates, the first document: every owner resolves
/// with `Document::node`, and the other documents' comments are theirs
/// (read through `Stream`).
#[test]
fn multi_document_recovering_parse_explains_the_first_document() {
    let src = "# h0\na # c0\n# f0\n---\nb # c1\n# f1\n";
    let parsed = Document::parse_recovering(src);
    assert!(parsed.has_errors(), "two documents in a Document");
    let d = parsed.into_value();
    let texts: Vec<String> = d
        .explain_comments()
        .unwrap()
        .iter()
        .map(|e| format!("{} r{}", e.text(), e.rule().number()))
        .collect();
    // `# f0` is before `---`: the second document's header (rule 1).
    assert_eq!(texts, ["# h0 r3", "# c0 r5"].map(String::from));
    for e in d.explain_comments().unwrap() {
        if let CommentOwner::Leading(n) | CommentOwner::Trailing(n) | CommentOwner::Footer(n) =
            e.owner()
        {
            assert!(d.node(n).is_ok(), "{e}");
        }
    }
    // The second document's comments are read through the stream.
    let s = Stream::parse(src).unwrap();
    let second = s.document(1).unwrap();
    let ids = second.comment_ids();
    assert_eq!(ids.len(), 3);
    for id in ids {
        let e = second.explain_comment(id).unwrap();
        if let CommentOwner::Trailing(n) = e.owner() {
            assert_eq!(second.node(n).unwrap().text(), "b");
        }
    }
    // The first document of the stream lists what the recovering document
    // lists.
    assert_eq!(
        s.document(0).unwrap().comment_ids().len(),
        d.comment_ids().len()
    );
}

/// Owners in restricted documents are nodes navigation
/// resolves, error regions included (an item that is an error region leads
/// or trails like any item); never the empty placeholder the parser puts
/// right after an error region (it has no bytes and ends its collection:
/// no rule reaches it, rulebook section 5).
#[test]
fn owners_in_recovered_documents_are_navigable_nodes() {
    let cases = [
        // `# lead` leads the item that is an error region.
        ("- 1\n# lead\n- 'x\n", "# lead", NodeKind::Error),
        ("a: 1 # t\nb: 'x\n", "# t", NodeKind::Scalar),
        ("k:\n  # lead\n  - 'x\n", "# lead", NodeKind::Error),
    ];
    for (src, text, kind) in cases {
        let d = Document::parse_recovering(src).into_value();
        assert!(d.has_errors(), "{src:?}");
        let all = d.explain_comments().unwrap();
        let e = all
            .iter()
            .find(|e| e.text() == text)
            .unwrap_or_else(|| panic!("{src:?}: {all:?}"));
        let (CommentOwner::Leading(n) | CommentOwner::Trailing(n)) = e.owner() else {
            panic!("{src:?}: {e}")
        };
        let node = d.node(n).unwrap();
        assert_eq!(node.kind(), kind, "{src:?}: {e}");
        assert_eq!(e.owner_span(), Some(node.span()), "{src:?}");
        assert!(!node.span().is_empty(), "{src:?}: no placeholder owner");
    }
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PIECES: &[&str] = &[
    "a",
    "b",
    "k: ",
    "- ",
    "? ",
    ": ",
    "[",
    "]",
    "{",
    "}",
    ", ",
    "# c",
    "#",
    "\n",
    "\n",
    "\n  ",
    "\n# c\n",
    "\n  # c\n",
    "\n\n",
    "  ",
    "---",
    "--- ",
    "...",
    "&x ",
    "&",
    "!t ",
    "*x",
    "|",
    "'q'",
    "%YAML 1.2",
    "%FOO",
    "\t",
    "x: |\n  t\n",
    "<<: *x",
];

/// Over a deterministic fuzz of recovered
/// inputs, every id `comment_ids` lists is explained by `explain_comment`
/// exactly as `explain_comments` explains it, every owner resolves with
/// `node`, and a stream's documents share out every comment of the input.
#[test]
fn public_explanations_agree_fuzz() {
    let n = std::env::var("ONEYAML_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_000);
    let mut rng = Rng(0x0DDB_1A5E_5BAD_5EED);
    let mut explained = 0usize;
    for _ in 0..n {
        let len = 1 + rng.below(20);
        let src: String = (0..len).map(|_| PIECES[rng.below(PIECES.len())]).collect();
        let mut docs = vec![Document::parse_recovering(src.as_str()).into_value()];
        let stream = Stream::parse_recovering(src.as_str()).into_value();
        docs.extend(stream.documents());
        for d in &docs {
            let ids = d.comment_ids();
            let all = d.explain_comments().unwrap();
            assert_eq!(ids.len(), all.len(), "{src:?}");
            for (id, e) in ids.iter().zip(&all) {
                assert_eq!(d.explain_comment(*id).as_ref(), Ok(e), "{src:?}");
                if let CommentOwner::Leading(o)
                | CommentOwner::Trailing(o)
                | CommentOwner::Footer(o) = e.owner()
                {
                    assert!(d.node(o).is_ok(), "{src:?}: {e}");
                }
                explained += 1;
            }
        }
        let shared: usize = stream.documents().map(|d| d.comment_ids().len()).sum();
        if !stream.is_empty() {
            let comments = stream
                .syntax()
                .root()
                .descendants()
                .filter(|t| t.kind() == oneyaml_syntax::tree::SyntaxKind::Comment)
                .count();
            assert_eq!(shared, comments, "{src:?}: stream partition");
        }
    }
    assert!(explained > n / 2, "{explained} explained");
}
