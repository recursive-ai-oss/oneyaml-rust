//! Detached forests of elements: what an edit removes and what it inserts
//! (subtrees). A fragment built from text goes through the one parser and
//! tree builder and is copied out of the tree they built: there is
//! no second parser.

use oneyaml_core::{Diagnostic, Settings, Severity};

use super::arena::{Arena, shift_span};
use super::{NONE, SyntaxKind, SyntaxTree, TreeEditError};

/// A detached, owned sequence of sibling elements (a forest) with their
/// subtrees: what [`SyntaxTree::remove`] returns and what
/// [`SyntaxTree::insert_before`] and friends insert.
///
/// Its bytes are [`Fragment::text`]; its elements keep their kinds,
/// block scalar headers, resolved tags, and diagnostics (spans relative to
/// [`Fragment::text`]). Building one from text parses it with the same
/// parser as [`SyntaxTree::parse_with`] ([`Fragment::parse_with`]), or
/// copies a sibling range out of any tree ([`SyntaxTree::extract`]).
///
/// Inserting consumes the fragment, so one fragment is inserted at most
/// once; a clone inserts an independent copy with its own handles.
///
/// ```compile_fail
/// use oneyaml_core::Settings;
/// use oneyaml_syntax::tree::{Fragment, SyntaxTree};
/// let mut tree = SyntaxTree::parse("[a]");
/// let seq = tree.root().first_child().unwrap().first_child().unwrap().id();
/// let f = Fragment::parse_with("b", tree.settings()).unwrap();
/// tree.append_child(seq, f).unwrap();
/// tree.append_child(seq, f).unwrap(); // use of moved value
/// ```
#[must_use = "a detached fragment is dropped unless it is inserted"]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Fragment {
    pub(super) arena: Arena,
    /// The `%TAG` directives (their text, one per line) of the document
    /// its tags were resolved in; empty outside any.
    pub(super) context: String,
    /// A tag property lies outside every document of the fragment, so its
    /// meaning depends on `context`.
    pub(super) has_tags: bool,
}

impl Fragment {
    /// [`Fragment::parse_with`] under the default [`Settings`] (as
    /// [`SyntaxTree::parse`]).
    ///
    /// # Errors
    ///
    /// As [`Fragment::parse_with`].
    pub fn parse(text: &str) -> Result<Self, TreeEditError> {
        Self::parse_with(text, &Settings::new())
    }

    /// Parses `text` under `settings` (normally the target tree's own,
    /// [`SyntaxTree::settings`]) and returns its top-level elements:
    /// the stream's children, with each implicit document (no directives,
    /// no `---`/`...`) replaced by its children. So `"x: 1"` gives one
    /// [`SyntaxKind::BlockMapping`], `"\n  "` a line break and white space,
    /// and an explicit document stays a [`SyntaxKind::Document`].
    ///
    /// The caller supplies text that is valid where it will be inserted,
    /// indentation included; see [`Fragment::unwrap_root`] to take a
    /// collection's entries instead of the collection. Warnings attached to
    /// an unwrapped document or to the end of the text are not kept.
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Syntax`] with the code of the first error when `text`
    /// does not parse cleanly.
    pub fn parse_with(text: &str, settings: &Settings) -> Result<Self, TreeEditError> {
        let tree = SyntaxTree::parse_with(text, settings);
        if let Some(d) = tree.diagnostics().find(|d| d.severity == Severity::Error) {
            return Err(TreeEditError::Syntax { code: d.code });
        }
        let mut roots = Vec::new();
        for c in tree.root().children() {
            let implicit = c.kind() == SyntaxKind::Document
                && !c.children().any(|d| {
                    matches!(
                        d.kind(),
                        SyntaxKind::DocumentStartMarker
                            | SyntaxKind::DocumentEndMarker
                            | SyntaxKind::YamlDirective
                            | SyntaxKind::TagDirective
                            | SyntaxKind::ReservedDirective
                    )
                });
            if implicit {
                roots.extend(c.children().map(|n| n.slot()));
            } else {
                roots.push(c.slot());
            }
        }
        let (arena, has_tags) = tree.copy_forest(&roots)?;
        Ok(Self {
            arena,
            context: String::new(),
            has_tags,
        })
    }

    /// The fragment's bytes: what inserting it adds to the tree's text.
    #[must_use]
    pub fn text(&self) -> &str {
        self.arena.source.get(..self.arena.tiled).unwrap_or("")
    }

    /// Number of elements, all subtrees included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.arena.kinds.len()
    }

    /// `true` for a fragment with no element (e.g. parsed from `""`),
    /// which cannot be inserted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.arena.kinds.is_empty()
    }

    /// The kinds of the top-level elements, in order.
    pub fn root_kinds(&self) -> impl Iterator<Item = SyntaxKind> + '_ {
        let flat = self.arena.flat();
        flat.roots().filter_map(move |i| flat.kind(i))
    }

    /// Number of top-level elements.
    #[must_use]
    #[doc(hidden)]
    pub fn root_count(&self) -> usize {
        self.arena.flat().roots().count()
    }

    /// The diagnostics its elements carry, spans relative to
    /// [`Fragment::text`]; one past `max_diagnostics` is a skeleton
    /// ([`SyntaxTree::is_skeleton`]).
    pub fn diagnostics(
        &self,
    ) -> impl ExactSizeIterator<Item = std::borrow::Cow<'_, Diagnostic>> + '_ {
        self.arena.diagnostics.iter().map(|(_, d)| d.view())
    }

    /// `true` when one of its elements carries an error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics().any(|d| d.severity == Severity::Error)
    }

    /// Replaces top-level element `index` by its children (O(fragment)):
    /// e.g. the entries of a parsed mapping, to add them to another
    /// mapping. A warning attached to the element moves to its first child.
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Position`] when there is no top-level element `index`;
    /// [`TreeEditError::ReadOnly`] when it is a token or an error node.
    pub fn unwrap_root(self, index: usize) -> Result<Self, TreeEditError> {
        let flat = self.arena.flat();
        let roots: Vec<usize> = flat.roots().collect();
        let p = *roots.get(index).ok_or(TreeEditError::Position {
            position: index,
            len: roots.len(),
        })?;
        let kind = flat.kind(p).ok_or(TreeEditError::Internal)?;
        if kind.is_token() || kind == SyntaxKind::Error {
            return Err(TreeEditError::ReadOnly { kind });
        }
        let p32 = u32::try_from(p).map_err(|_| TreeEditError::Internal)?;
        // The element's warnings move to its first
        // child (the index after it once it is gone). Without a child they
        // would land on the next root or be dropped, so that is refused
        // (nothing destroyed silently). Parsed nodes always have a
        // child; only a hand-built arena lacks one.
        if flat.first_child(p).is_none() && !flat.diagnostics_at(p).is_empty() {
            return Err(TreeEditError::ReadOnly { kind });
        }
        let down = |i: u32| {
            if i > p32 && i != NONE {
                i.saturating_sub(1)
            } else {
                i
            }
        };
        let a = &self.arena;
        let mut out = Arena {
            source: a.source.clone(),
            tiled: a.tiled,
            ..Arena::default()
        };
        for (i, ((k, parent), word)) in a.kinds.iter().zip(&a.parents).zip(&a.words).enumerate() {
            if i == p {
                continue;
            }
            out.kinds.push(*k);
            out.parents
                .push(if *parent == p32 { NONE } else { down(*parent) });
            out.words
                .push(if k.is_token() { *word } else { down(*word) });
        }
        let len = u32::try_from(out.kinds.len()).map_err(|_| TreeEditError::Internal)?;
        out.extras = a
            .extras
            .iter()
            .filter(|(i, _)| *i != p32)
            .map(|(i, e)| (down(*i), *e))
            .collect();
        out.diagnostics = a
            .diagnostics
            .iter()
            .map(|(i, d)| (down(*i), d.clone()))
            .filter(|(i, _)| *i < len)
            .collect();
        let has_tags = tags_outside_documents(&out);
        Ok(Self {
            arena: out,
            context: self.context,
            has_tags,
        })
    }

    /// This fragment followed by `other` (O(both)).
    ///
    /// # Errors
    ///
    /// [`TreeEditError::TagContext`] when both hold tags resolved under
    /// different `%TAG` directives; [`TreeEditError::Capacity`] past 4 GiB or
    /// 2³² elements.
    pub fn concat(self, other: Self) -> Result<Self, TreeEditError> {
        if self.has_tags && other.has_tags && self.context != other.context {
            return Err(TreeEditError::TagContext);
        }
        let context = if self.has_tags || !other.has_tags {
            self.context
        } else {
            other.context
        };
        let (a, b) = (self.arena, other.arena);
        let big = |n: usize| u32::try_from(n).ok().filter(|n| *n < NONE);
        let ta = a.tiled;
        let tb = b.tiled;
        let tail_a = a.source.get(ta..).unwrap_or("");
        let tail_b = b.source.get(tb..).unwrap_or("");
        let total = a.source.len().saturating_add(b.source.len());
        big(total).ok_or(TreeEditError::Capacity)?;
        let na = big(a.kinds.len()).ok_or(TreeEditError::Capacity)?;
        big(a.kinds.len().saturating_add(b.kinds.len())).ok_or(TreeEditError::Capacity)?;
        let ta32 = big(ta).ok_or(TreeEditError::Capacity)?;
        let mut source = String::with_capacity(total);
        source.push_str(a.source.get(..ta).unwrap_or(""));
        source.push_str(b.source.get(..tb).unwrap_or(""));
        source.push_str(tail_a);
        source.push_str(tail_b);
        let tiled = ta.saturating_add(tb);
        let mut out = Arena {
            source,
            tiled,
            kinds: a.kinds,
            parents: a.parents,
            words: a.words,
            extras: Vec::new(),
            diagnostics: a.diagnostics,
        };
        out.extras = a
            .extras
            .into_iter()
            .map(|(i, mut e)| {
                e.tag_prefix = e.tag_prefix.map(|p| p.saturating_add(tb));
                (i, e)
            })
            .collect();
        for ((k, parent), word) in b.kinds.iter().zip(&b.parents).zip(&b.words) {
            out.kinds.push(*k);
            out.parents.push(if *parent == NONE {
                NONE
            } else {
                parent.saturating_add(na)
            });
            out.words.push(if k.is_token() {
                word.saturating_add(ta32)
            } else {
                word.saturating_add(na)
            });
        }
        let tail_shift = ta.saturating_add(tail_a.len());
        for (i, mut e) in b.extras {
            e.tag_prefix = e.tag_prefix.map(|p| p.saturating_add(tail_shift));
            if let Some(h) = e.block.as_mut() {
                h.span = shift_span(h.span, 0, ta);
            }
            out.extras.push((i.saturating_add(na), e));
        }
        for (i, mut d) in b.diagnostics {
            d.shift(0, ta);
            out.diagnostics.push((i.saturating_add(na), d));
        }
        let has_tags = self.has_tags || other.has_tags;
        Ok(Self {
            arena: out,
            context,
            has_tags,
        })
    }
}

impl Fragment {
    /// The fragment's text ends with a line break.
    pub(super) fn ends_line(&self) -> bool {
        self.text().ends_with(['\n', '\r'])
    }

    /// When the last top-level element is a line break token
    /// (and not the only one), the fragment without it and that break as
    /// a fragment of its own; else the fragment unchanged and `None`.
    /// O(fragment).
    pub(super) fn split_last_break(self) -> Result<(Self, Option<Self>), TreeEditError> {
        let flat = self.arena.flat();
        let Some(last) = flat.roots().last() else {
            return Ok((self, None));
        };
        if flat.kind(last) != Some(SyntaxKind::LineBreak) || last == 0 {
            return Ok((self, None));
        }
        // A line break token is a leaf: the last element of the arena.
        if last.checked_add(1) != Some(flat.len()) {
            return Err(TreeEditError::Internal);
        }
        let span = flat.token_span(last);
        let text = flat.token_text(last).to_owned();
        let diagnostics = flat.diagnostics_at(last).to_vec();
        let mut body = self.arena;
        let old_tiled = body.tiled;
        let len = span.end.saturating_sub(span.start);
        // Nothing destroyed silently; a diagnostic on the break
        // stays with it.
        let lo = body
            .diagnostics
            .partition_point(|(i, _)| (*i as usize) < last);
        body.diagnostics.truncate(lo);
        body.kinds.truncate(last);
        body.parents.truncate(last);
        body.words.truncate(last);
        let tail = body.source.get(old_tiled..).unwrap_or("").to_owned();
        body.source.truncate(span.start);
        body.source.push_str(&tail);
        body.tiled = span.start;
        for (_, e) in &mut body.extras {
            e.tag_prefix = e.tag_prefix.map(|p| p.saturating_sub(len));
        }
        let has_tags = tags_outside_documents(&body);
        let mut brk = Arena {
            source: text,
            tiled: len,
            kinds: vec![SyntaxKind::LineBreak],
            parents: vec![NONE],
            words: vec![0],
            ..Arena::default()
        };
        brk.diagnostics = diagnostics
            .into_iter()
            .map(|(_, mut d)| {
                d.shift(span.start, 0);
                (0, d)
            })
            .collect();
        Ok((
            Self {
                arena: body,
                context: self.context,
                has_tags,
            },
            Some(Self {
                arena: brk,
                context: String::new(),
                has_tags: false,
            }),
        ))
    }
}

/// `true` when a tag token of `arena` lies outside every document node
/// (O(n), an explicit stack of open documents).
pub(super) fn tags_outside_documents(arena: &Arena) -> bool {
    let flat = arena.flat();
    let mut docs: Vec<usize> = Vec::new();
    for i in 0..flat.len() {
        while docs.last().is_some_and(|end| *end <= i) {
            docs.pop();
        }
        match flat.kind(i) {
            Some(SyntaxKind::Document) => docs.push(flat.end(i)),
            Some(SyntaxKind::Tag) if docs.is_empty() => return true,
            _ => {}
        }
    }
    false
}
