//! Exact comment-owner accounting for the edit fuzzes (the rulebook's edit
//! semantics, `docs/comment-attachment.md`).
//!
//! Every comment is read before and after an accepted call with its
//! **rulebook** owner (explicit owners set aside) as (text, owner kind,
//! owner node). Its owner afterwards must be exactly the one before —
//! the same node handle, or, when the call rewrote the comment, a new
//! handle with the same text and owner — except for these documented
//! transitions (one per [`Op`]), and nothing else:
//!
//! * **plain** (insertions, and anything else): no comment changes owner,
//!   none is added or lost;
//! * **taken** (`Op::Remove`): a removal's `Removed` holds exactly the
//!   comments owned by nodes of the removed entry (its key, its value,
//!   anything inside), and they are gone from the document;
//! * **keep lead** (`Op::Remove`, `keep_lead`): `remove_keep_comments` gives
//!   the entry's leading comments to the next entry, or the collection's
//!   footer, or — for the last entry of a root collection — the document
//!   footer;
//! * **collapse** (`Op::Remove` of an only entry, the collection rewritten
//!   as `{}`/`[]`): the collection's leading and trailing comments are the
//!   new node's; a root collection's footers become the document footer
//!   (rule 7); a nested collection's footers stay on their lines below the
//!   new `{}`/`[]`; an empty flow collection owns no footer, so the next
//!   node after it leads them, or, when none follows at that level, the
//!   enclosing collection owns them as its footer (and so on outwards),
//!   never the document's header or footer;
//! * **replace** (`Op::Replace`): the old value's inner comments (owned
//!   inside it, or its footer) go; its own leading and trailing comments
//!   are owned by the new value or by its entry's key (a comment written
//!   back on the key's line); the comment lines above a replaced root
//!   stay above the new root and are owned by it or a node inside it;
//! * **rewrite** (`Op::Rewrite`, `set_anchor`): every comment owned by the
//!   node or inside it is owned by the corresponding node (same place) of
//!   the rewritten node;
//! * **move** (`Op::Move`): the moved entry's comments are owned by the
//!   corresponding node (same place inside the entry) of the entry at its
//!   new index;
//! * **re-insert** (`Op::Reinsert`): the `Removed`'s comments are owned by
//!   the corresponding node of the re-inserted entry; when the target is
//!   a single-pair flow collection rewritten as a mapping (`[a: b]`), the
//!   comments owned inside it are owned by the corresponding nodes of the
//!   new mapping.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use oneyaml::{CommentId, CommentOwner, Document, NodeId, NodeKind, Span};

/// Every comment by handle, with its rulebook owner and text, in document
/// order.
pub type Snap = Vec<(CommentId, CommentOwner, String)>;

/// The rulebook owners of `doc`'s comments (explicit owners set aside).
pub fn snap(doc: &Document) -> Snap {
    let mut c = doc.clone();
    for id in c.comment_ids() {
        let _ = c.detach_comment(id);
    }
    c.explain_comments()
        .unwrap()
        .iter()
        .map(|e| (e.id(), e.owner(), e.text().to_owned()))
        .collect()
}

/// The node tree of a document: children in order (a mapping's are key,
/// value, key, value, …), parents, and which nodes are mappings.
#[derive(Default, Clone)]
pub struct Shape {
    pub root: Option<NodeId>,
    children: HashMap<NodeId, Vec<NodeId>>,
    parent: HashMap<NodeId, NodeId>,
    maps: HashSet<NodeId>,
    /// The text, each node's span, and each comment's span (also
    /// where an entry starts, for `remove_keep_comments`).
    text: String,
    spans: HashMap<NodeId, Span>,
    comments: HashMap<CommentId, Span>,
}

impl Shape {
    /// A loop, not recursion; aliases are leaves.
    pub fn of(doc: &Document) -> Self {
        // Spans are the stream's offsets for a
        // document of a `Stream`; `text` is the document's own region, so
        // every span is taken relative to the region's start.
        let base = doc.span().start;
        let rel = |s: Span| Span::new(s.start.saturating_sub(base), s.end.saturating_sub(base));
        let mut s = Self {
            root: doc.root().map(|r| r.id()),
            text: doc.to_string(),
            comments: doc
                .explain_comments()
                .unwrap()
                .iter()
                .map(|e| (e.id(), rel(e.span())))
                .collect(),
            ..Self::default()
        };
        let mut stack: Vec<_> = doc.root().into_iter().collect();
        while let Some(n) = stack.pop() {
            s.spans.insert(n.id(), rel(n.span()));
            let mut kids = Vec::new();
            if n.kind() == NodeKind::Mapping {
                if let Some(m) = n.as_mapping() {
                    s.maps.insert(n.id());
                    for (k, v) in m.iter() {
                        kids.push(k);
                        kids.push(v);
                    }
                }
            } else if n.kind() == NodeKind::Sequence {
                if let Some(q) = n.as_sequence() {
                    kids.extend(q.iter());
                }
            }
            for k in &kids {
                s.parent.insert(k.id(), n.id());
            }
            s.children
                .insert(n.id(), kids.iter().map(|k| k.id()).collect());
            stack.extend(kids);
        }
        s
    }

    /// The entries of collection `c`: (key, value).
    pub fn entries(&self, c: NodeId) -> Vec<(Option<NodeId>, NodeId)> {
        let kids = self.children.get(&c).cloned().unwrap_or_default();
        if self.maps.contains(&c) {
            kids.chunks(2).map(|p| (Some(p[0]), p[1])).collect()
        } else {
            kids.into_iter().map(|v| (None, v)).collect()
        }
    }

    /// `n` and every node inside it.
    pub fn subtree(&self, n: NodeId) -> HashSet<NodeId> {
        let mut out = HashSet::new();
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            if out.insert(x) {
                stack.extend(self.children.get(&x).into_iter().flatten().copied());
            }
        }
        out
    }

    /// The place of `n` inside entry `e`: `[0]` its key, `[1]` its value,
    /// then child indices.
    pub fn rel(&self, e: (Option<NodeId>, NodeId), n: NodeId) -> Option<Vec<usize>> {
        let mut path = Vec::new();
        let mut at = n;
        loop {
            if Some(at) == e.0 {
                path.push(0);
                break;
            }
            if at == e.1 {
                path.push(1);
                break;
            }
            let p = *self.parent.get(&at)?;
            let i = self.children.get(&p)?.iter().position(|x| *x == at)?;
            path.push(i);
            at = p;
        }
        path.reverse();
        Some(path)
    }

    /// The node at `rel` inside entry `e` (see [`Shape::rel`]).
    pub fn resolve(&self, e: (Option<NodeId>, NodeId), rel: &[usize]) -> Option<NodeId> {
        let (first, rest) = rel.split_first()?;
        let mut at = if *first == 0 { e.0? } else { e.1 };
        for i in rest {
            at = *self.children.get(&at)?.get(*i)?;
        }
        Some(at)
    }

    /// Whether comment `c` is written above entry `e`: before the entry's
    /// first token — its first node, or the `? `/`- ` indicator or the
    /// properties before it (other comments and white space between).
    /// `remove_keep_comments` leaves the entry's leading comments written
    /// there; one written inside the entry goes with it.
    pub fn above_entry(&self, e: (Option<NodeId>, NodeId), c: CommentId) -> bool {
        let (Some(cs), Some(ls)) = (self.comments.get(&c), self.spans.get(&e.0.unwrap_or(e.1)))
        else {
            return false;
        };
        let b = self.text.as_bytes();
        let at = |i: usize| i.checked_sub(1).and_then(|j| b.get(j).copied());
        let ends: HashMap<usize, usize> =
            self.comments.values().map(|s| (s.end, s.start)).collect();
        let mut start = ls.start;
        let mut p = ls.start;
        // The first entry of a collection that starts where the
        // entry does (`?⏎# c⏎  a: 1`, the explicit key's mapping): an
        // indicator or properties before it are that collection's.
        let mine = self.subtree(e.0.unwrap_or(e.1));
        let first_of_enclosing = self.spans.iter().any(|(id, s)| {
            *id != e.1 && !mine.contains(id) && s.start == ls.start && s.end >= ls.end
        });
        loop {
            // White space (and a BOM) back to something else.
            while let Some(ch) = at(p) {
                if ends.contains_key(&p) {
                    // (A comment ending in white space.)
                    break;
                } else if matches!(ch, b' ' | b'\t' | b'\n' | b'\r') {
                    p -= 1;
                } else if p >= 3 && b.get(p - 3..p) == Some("\u{feff}".as_bytes()) {
                    p -= 3;
                } else {
                    break;
                }
            }
            if let Some(s) = ends.get(&p) {
                p = *s;
                continue;
            }
            // A property or an indicator of the entry.
            let mut w = p;
            while at(w).is_some_and(|ch| !matches!(ch, b' ' | b'\t' | b'\n' | b'\r')) {
                w -= 1;
            }
            let word = &b[w..p];
            let lone = |ch: u8| word == [ch];
            // (Not another node's text, e.g. an anchored empty value; nor
            // another entry's indicator: the `?` of an empty key
            // written without `:` before this entry, `?⏎# c⏎k: v`, whose
            // empty key lies between it and this entry.)
            let other = self.spans.iter().any(|(id, s)| {
                Some(*id) != e.0
                    && *id != e.1
                    && s.end <= ls.start
                    && ((s.start <= w && w < s.end) || (p <= s.start && s.start < ls.start))
            });
            // (Nor an enclosing collection's properties:
            // `!t # c⏎- a⏎`, the tag starts the sequence.)
            let prop = matches!(word.first(), Some(b'&' | b'!'));
            // An oracle fix: the properties may be several,
            // `&x !⏎# c⏎k: v`, comment lines between them too (`&x # c⏎!`):
            // the collection starts at the first.
            let only_props = |t: &[u8]| {
                t.split(|c| matches!(c, b'\n' | b'\r')).all(|line| {
                    // (A comment runs to the end of its line.)
                    let cut = line
                        .iter()
                        .enumerate()
                        .position(|(i, c)| {
                            *c == b'#' && (i == 0 || matches!(line[i - 1], b' ' | b'\t'))
                        })
                        .unwrap_or(line.len());
                    line[..cut]
                        .split(|c| matches!(c, b' ' | b'\t'))
                        .all(|x| matches!(x.first(), None | Some(b'&' | b'!')))
                })
            };
            let enclosing = prop
                && self.spans.iter().any(|(id, s)| {
                    Some(*id) != e.0
                        && *id != e.1
                        && s.start <= w
                        && b.get(s.start..w).is_some_and(only_props)
                });
            if !other && !enclosing && !first_of_enclosing && (prop || lone(b'?') || lone(b'-')) {
                p = w;
                start = w;
                continue;
            }
            break;
        }
        cs.end <= start
    }

    /// Whether mapping `m` is a single-pair flow mapping (`[k: v]`): an
    /// item of a flow sequence, not written with braces.
    pub fn is_pair(&self, m: NodeId) -> bool {
        let text = |n: NodeId| {
            let sp = self.spans.get(&n)?;
            self.text.get(sp.start..sp.end)
        };
        let flow_parent = self.parent(m).and_then(text).is_some_and(|t| {
            // (Past the sequence's properties, and past comments between
            // them and the bracket: an oracle fix,
            // `! # c⏎[k: v]`.)
            t.split(['\n', '\r'])
                .flat_map(|l| {
                    let mut words = Vec::new();
                    for w in l.split_whitespace() {
                        if w.starts_with('#') {
                            break;
                        }
                        words.push(w);
                    }
                    words
                })
                .find(|w| !w.starts_with(['&', '!']))
                .is_some_and(|w| w.starts_with('['))
        });
        self.maps.contains(&m) && flow_parent && text(m).is_some_and(|t| !t.starts_with('{'))
    }

    /// Whether comment `c` lies outside node `n`'s text.
    pub fn outside(&self, n: NodeId, c: CommentId) -> bool {
        match (self.spans.get(&n), self.comments.get(&c)) {
            (Some(ns), Some(cs)) => cs.end <= ns.start || ns.end <= cs.start,
            _ => false,
        }
    }

    pub fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.parent.get(&n).copied()
    }

    fn index_in_parent(&self, n: NodeId) -> Option<(NodeId, usize)> {
        let p = self.parent(n)?;
        let i = self.children.get(&p)?.iter().position(|x| *x == n)?;
        Some((p, i))
    }

    fn child(&self, p: NodeId, i: usize) -> Option<NodeId> {
        self.children.get(&p)?.get(i).copied()
    }
}

/// A comment a `Removed` carries: its text and its owner's kind and place
/// inside the removed entry.
#[derive(Clone, Debug)]
pub struct Carried {
    pub text: String,
    pub kind: u8,
    pub rel: Vec<usize>,
}

/// What an accepted call was, for the accounting.
#[derive(Clone, Debug, Default)]
pub enum Op {
    /// Nothing is taken, replaced, moved, or brought.
    #[default]
    Plain,
    /// A removal of an entry of `coll` (the one whose value went stale);
    /// `taken`: the `Removed`'s comment handles; `keep_lead`:
    /// `remove_keep_comments`.
    Remove {
        coll: NodeId,
        taken: Vec<CommentId>,
        keep_lead: bool,
    },
    /// Node `old` replaced by `new`.
    Replace { old: NodeId, new: Option<NodeId> },
    /// Node `old` rewritten as `new`, the same structure (`set_anchor`):
    /// every comment owned inside it is owned by the corresponding node.
    Rewrite { old: NodeId, new: NodeId },
    /// `move_entry(from, to)` in `coll`.
    Move {
        coll: NodeId,
        from: usize,
        to: usize,
    },
    /// A `Removed` put into `coll` at `index` (`None`: appended), carrying
    /// these comments.
    Reinsert {
        coll: NodeId,
        index: Option<usize>,
        carried: Vec<Carried>,
    },
}

fn kind_of(o: CommentOwner) -> (u8, Option<NodeId>) {
    match o {
        CommentOwner::DocumentHeader => (0, None),
        CommentOwner::DocumentFooter => (1, None),
        CommentOwner::Leading(n) => (2, Some(n)),
        CommentOwner::Trailing(n) => (3, Some(n)),
        CommentOwner::Footer(n) => (4, Some(n)),
        _ => (9, None),
    }
}

fn owner(kind: u8, n: NodeId) -> CommentOwner {
    match kind {
        2 => CommentOwner::Leading(n),
        3 => CommentOwner::Trailing(n),
        _ => CommentOwner::Footer(n),
    }
}

/// What [`check`] saw.
#[derive(Default)]
pub struct Report {
    /// For a removal, the comments its `Removed` carries (for a later
    /// re-insertion).
    pub carried: Vec<Carried>,
    /// Comments whose owner the call changed by a documented transition.
    pub moved: usize,
    /// Comments checked.
    pub checked: usize,
}

/// Checks one accepted call (panics with `ctx` on any other transition).
pub fn check(before: &Snap, bs: &Shape, doc: &Document, op: &Op, ctx: &str) -> Report {
    let text = doc.to_string();
    let after = snap(doc);
    let aft = Shape::of(doc);
    let fail = |m: String| -> ! { panic!("owner accounting: {m}: {ctx}\nafter: {text:?}") };
    // Expected fate of every comment before: `None` gone, `Some(owner)`.
    let mut fate: Vec<(CommentId, String, Option<Vec<CommentOwner>>)> = Vec::new();
    let mut carried_out = Vec::new();
    let mut brought: Vec<(String, Vec<CommentOwner>)> = Vec::new();
    let mut moved = 0usize;
    match op {
        Op::Remove {
            coll,
            taken,
            keep_lead,
        } => {
            let entries = bs.entries(*coll);
            let collapsed = doc.node(*coll).is_err();
            let gone: Vec<usize> = (0..entries.len())
                .filter(|i| doc.node(entries[*i].1).is_err())
                .collect();
            let at = match gone.as_slice() {
                [one] => *one,
                _ if collapsed && entries.len() == 1 => 0,
                _ => fail(format!("no removed entry found ({gone:?})")),
            };
            let e = entries[at];
            let mut inside = bs.subtree(e.1);
            if let Some(k) = e.0 {
                inside.extend(bs.subtree(k));
            }
            let lead_of = e.0.unwrap_or(e.1);
            let rehome = match entries.get(at + 1) {
                Some(n) => CommentOwner::Leading(n.0.unwrap_or(n.1)),
                None if bs.root == Some(*coll) => CommentOwner::DocumentFooter,
                None => CommentOwner::Footer(*coll),
            };
            // A collapsed collection's replacement.
            let new_coll = if collapsed {
                match bs.index_in_parent(*coll) {
                    Some((p, i)) => aft.child(p, i),
                    None => aft.root,
                }
            } else {
                None
            };
            let mut want_taken = Vec::new();
            for (id, o, x) in before {
                let (k, n) = kind_of(*o);
                let f = match n {
                    // A comment of the only entry written outside
                    // the collection (rule 9: `[ # c⏎, k: v]` leads the pair's
                    // key) is the rewritten collection's, as its own are.
                    Some(n)
                        if inside.contains(&n)
                            && collapsed
                            && bs.is_pair(*coll)
                            && bs.outside(*coll, *id) =>
                    {
                        match new_coll {
                            Some(c) => Some(owner(k, c)),
                            None => fail("no collapsed collection found".into()),
                        }
                    }
                    Some(n) if inside.contains(&n) => {
                        if *keep_lead
                            && *o == CommentOwner::Leading(lead_of)
                            && bs.above_entry(e, *id)
                        {
                            Some(rehome)
                        } else {
                            want_taken.push(*id);
                            carried_out.push(Carried {
                                text: x.clone(),
                                kind: k,
                                rel: bs.rel(e, n).unwrap(),
                            });
                            None
                        }
                    }
                    // A nested collapsed collection's footers stay where they
                    // are, and an empty flow collection owns no footer: the
                    // next node after the collection leads them, or, when
                    // none follows at that level, the enclosing collection
                    // owns them as its footer (and so on outwards).
                    Some(n) if n == *coll && collapsed && k == 4 && bs.root != Some(*coll) => {
                        let mut alts = Vec::new();
                        let mut at = *coll;
                        while let Some((p, i)) = bs.index_in_parent(at) {
                            if let Some(next) = bs.child(p, i + 1) {
                                alts.push(owner(2, next));
                                break;
                            }
                            alts.push(owner(4, p));
                            at = p;
                        }
                        fate.push((*id, x.clone(), Some(alts)));
                        continue;
                    }
                    Some(n) if n == *coll && collapsed => match (k, new_coll) {
                        (4, _) if bs.root == Some(*coll) => Some(CommentOwner::DocumentFooter),
                        (_, Some(c)) => Some(owner(k, c)),
                        (_, None) => fail("no collapsed collection found".into()),
                    },
                    _ => Some(*o),
                };
                fate.push((*id, x.clone(), f.map(|o| vec![o])));
            }
            if &want_taken != taken {
                fail(format!(
                    "the Removed holds {taken:?}, the entry owns {want_taken:?}"
                ));
            }
        }
        Op::Replace { old, new } => {
            let inner = bs.subtree(*old);
            // The new value's entry: its key (a same-line comment of a
            // `key: value` line is the entry's).
            let key = new.and_then(|nw| {
                let (p, i) = aft.index_in_parent(nw)?;
                aft.maps
                    .contains(&p)
                    .then(|| aft.child(p, i.checked_sub(1)?))?
            });
            for (id, o, x) in before {
                let (k, n) = kind_of(*o);
                let f = match n {
                    // Comment lines above a replaced root stay above the new
                    // root; it or a node inside it owns them.
                    Some(n)
                        if bs.root == Some(*old)
                            && k == 2
                            && inner.contains(&n)
                            && bs
                                .comments
                                .get(id)
                                .zip(bs.spans.get(old))
                                .is_some_and(|(c, s)| c.end <= s.start) =>
                    {
                        match new {
                            Some(new) => {
                                Some(aft.subtree(*new).into_iter().map(|m| owner(2, m)).collect())
                            }
                            None => fail(format!("{x:?}: no new node")),
                        }
                    }
                    Some(n) if n == *old && k != 4 => match new {
                        Some(new) => {
                            let mut alts = vec![owner(k, *new)];
                            alts.extend(key.map(|kk| owner(k, kk)));
                            Some(alts)
                        }
                        None => fail(format!("{x:?}: no new node")),
                    },
                    // A comment written outside a replaced
                    // single-pair flow mapping that leads its key (rule 9:
                    // `[ a⏎# c⏎, k: v]`) leads the item, so the new node's.
                    Some(n)
                        if inner.contains(&n)
                            && bs.is_pair(*old)
                            && bs.outside(*old, *id)
                            && k != 4 =>
                    {
                        match new {
                            Some(new) => Some(vec![owner(k, *new)]),
                            None => fail(format!("{x:?}: no new node")),
                        }
                    }
                    Some(n) if inner.contains(&n) => None,
                    _ => Some(vec![*o]),
                };
                fate.push((*id, x.clone(), f));
            }
        }
        Op::Rewrite { old, new } => {
            let inside = bs.subtree(*old);
            for (id, o, x) in before {
                let (k, n) = kind_of(*o);
                let f = match n {
                    Some(n) if inside.contains(&n) => {
                        let rel = bs.rel((None, *old), n).unwrap();
                        let target = aft.resolve((None, *new), &rel).unwrap_or_else(|| {
                            fail(format!("{x:?}: no rewritten node at {rel:?}"))
                        });
                        Some(owner(k, target))
                    }
                    _ => Some(*o),
                };
                fate.push((*id, x.clone(), f.map(|o| vec![o])));
            }
        }
        Op::Move { coll, from, to } => {
            let e = bs.entries(*coll)[*from];
            let ne = aft.entries(*coll).get(*to).copied();
            let mut inside = bs.subtree(e.1);
            if let Some(k) = e.0 {
                inside.extend(bs.subtree(k));
            }
            for (id, o, x) in before {
                let (k, n) = kind_of(*o);
                let f = match n {
                    Some(n) if inside.contains(&n) => {
                        let rel = bs.rel(e, n).unwrap();
                        let target = ne
                            .and_then(|ne| aft.resolve(ne, &rel))
                            .unwrap_or_else(|| fail(format!("{x:?}: no moved node at {rel:?}")));
                        Some(owner(k, target))
                    }
                    _ => Some(*o),
                };
                fate.push((*id, x.clone(), f.map(|o| vec![o])));
            }
        }
        Op::Reinsert {
            coll,
            index,
            carried,
        } => {
            // A single-pair flow collection (`[a: b]`) is rewritten as a
            // mapping holding both entries: its nodes (the pair's key and
            // value first) are the new mapping's.
            let (target, rewritten) = if doc.node(*coll).is_ok() {
                (*coll, None)
            } else {
                let t = match bs.index_in_parent(*coll) {
                    Some((p, i)) => aft.child(p, i),
                    None => aft.root,
                }
                .unwrap_or_else(|| fail("no rewritten collection".into()));
                (t, Some(t))
            };
            let es = aft.entries(target);
            // (An index past the end appends.)
            let ne = match index {
                Some(i) => es.get(*i).or(es.last()).copied(),
                None => es.last().copied(),
            }
            .unwrap_or_else(|| {
                fail(format!(
                    "no re-inserted entry in {target:?} ({} entries; {:?})",
                    es.len(),
                    doc.node(target).map(|n| (n.kind(), n.text().into_owned()))
                ))
            });
            for c in carried {
                let n = aft
                    .resolve(ne, &c.rel)
                    .unwrap_or_else(|| fail(format!("{:?}: no node at {:?}", c.text, c.rel)));
                brought.push((c.text.clone(), vec![owner(c.kind, n)]));
            }
            let inside = bs.subtree(*coll);
            for (id, o, x) in before {
                let (k, n) = kind_of(*o);
                let f = match (n, rewritten) {
                    (Some(n), Some(t)) if inside.contains(&n) => {
                        let rel = bs.rel((None, *coll), n).unwrap();
                        let m = aft
                            .resolve((None, t), &rel)
                            .unwrap_or_else(|| fail(format!("{x:?}: no node at {rel:?}")));
                        owner(k, m)
                    }
                    _ => *o,
                };
                fate.push((*id, x.clone(), Some(vec![f])));
            }
        }
        Op::Plain => {
            fate.extend(
                before
                    .iter()
                    .map(|(id, o, x)| (*id, x.clone(), Some(vec![*o]))),
            );
        }
    }
    let after_map: HashMap<CommentId, (CommentOwner, &str)> = after
        .iter()
        .map(|(id, o, x)| (*id, (*o, x.as_str())))
        .collect();
    let before_ids: HashSet<CommentId> = before.iter().map(|c| c.0).collect();
    moved += brought.len();
    let checked = fate.len() + brought.len();
    let olds: HashMap<CommentId, CommentOwner> = before.iter().map(|c| (c.0, c.1)).collect();
    let mut rewritten: Vec<(String, Vec<CommentOwner>)> = brought;
    for (id, x, f) in fate {
        if f.as_ref()
            .is_some_and(|o| olds.get(&id).is_none_or(|was| !o.contains(was)))
        {
            moved += 1;
        }
        match (f, after_map.get(&id)) {
            (None, Some(_)) => fail(format!("{x:?} should be gone")),
            (None, None) => {}
            (Some(o), Some((now, nx))) => {
                if *nx != x {
                    fail(format!("{x:?} changed its text to {nx:?}"));
                }
                if !o.contains(now) {
                    fail(format!("{x:?}: owner {now:?}, want {o:?}"));
                }
            }
            (Some(o), None) => rewritten.push((x, o)),
        }
    }
    let mut fresh: Vec<(String, CommentOwner)> = after
        .iter()
        .filter(|c| !before_ids.contains(&c.0))
        .map(|c| (c.2.clone(), c.1))
        .collect();
    for (x, o) in rewritten {
        match fresh.iter().position(|f| f.0 == x && o.contains(&f.1)) {
            Some(p) => {
                fresh.remove(p);
            }
            None => fail(format!(
                "{x:?} lost or re-owned (want {o:?}); new: {fresh:?}"
            )),
        }
    }
    if !fresh.is_empty() {
        fail(format!("comments made up: {fresh:?}"));
    }
    Report {
        carried: carried_out,
        moved,
        checked,
    }
}
