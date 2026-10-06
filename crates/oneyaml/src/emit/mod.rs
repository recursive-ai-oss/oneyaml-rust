//! The one emitter.
//!
//! [`emit_document`] writes one [`Value`] as a YAML document. It walks the
//! value with an explicit stack of [`Task`]s — never recursion — so any
//! depth is safe on a small stack; the depth it accepts is
//! bounded by `Limits::max_depth` only (`Y2005`). Scalar
//! presentation is decided by [`scalar`].
//!
//! The typed serializer (`crate::ser::stream`) writes through the
//! same [`Emitter`] primitives — the [`Slot`] positions, block openings,
//! string nodes, keys, tags, and document starts below — as serde's calls
//! arrive, so both paths share every layout and quoting rule (one
//! interpretation) and write the same bytes for the same data.
//!
//! # Layout
//!
//! Non-empty collections are block collections; empty ones are `[]`/`{}`.
//! A block mapping value that is a collection starts on the next line,
//! indented by `Emit::indent` (sequences only with
//! `Emit::indent_sequences`); a collection inside a sequence entry or an
//! explicit key/value starts on the entry's own line (`- - a`, `- a: 1`).
//! A key that cannot be implicit — a collection (`serde_yaml` layout:
//! a non-empty one), a multi-line string, a too-long scalar — uses the
//! explicit `? key` / `: value` form.
//!
//! **Keys** (2026-09-27): a scalar key of any type is
//! implicit — `1: x`, `true: x`, `null: x`, `1.5: x` — so only collection
//! (complex) keys use `? ` ("non-string keys" means
//! "non-scalar keys".
//!
//! # Output size
//!
//! Block output is O(nodes × depth × indent): every line of a nested
//! block collection repeats its indentation, one `Emit::indent` per level.
//! Nested sequences stay on one line (`- - - x`), but nested mappings put
//! each level on its own, more indented line, so a chain of `d` mappings
//! writes about `indent × d² / 2` bytes: a 5 KB flow document
//! `{a: {a: …}}` 1024 levels deep re-serializes to about 1 MB.
//!
//! Both the `Value` walk and the streaming serializer check
//! `Limits::max_depth` **before** writing what is too deep (`Y2005`; for a
//! `Value`, one walk that stops at the limit), so refusing a deep value
//! costs O(`max_depth`), never its output. With a depth limit the output
//! is bounded by nodes × `max_depth` × `Emit::MAX_INDENT` (`Y1012` above
//! it). **Without one** (`Limits::unlimited()`) nothing bounds the depth,
//! and the quadratic output is the caller's to afford: a 100,000-level
//! mapping chain is about 10 GB of text.

pub(crate) mod scalar;

use oneyaml_core::{Diagnostic, Label, Limits, Schema, Settings, Span, code};

use crate::error::Error;
use crate::resolve::conflict::Conflict;
use crate::value::{MappingIter, TagStr, Value};
use scalar::{BlockIndent, Rules};

/// Checks the settings for output and builds the scalar rules.
///
/// * `Emit::indent(0)` cannot express nesting: `Y1012` (the shared
///   conflict check); above `Emit::MAX_INDENT`, `Y1012` too (from
///   [`Rules::new`]: never a panic or an allocation blow-up).
/// * The JSON and Failsafe schemas cannot read Core output back as the
///   same values (plain strings are errors under JSON, and every scalar is
///   a string under Failsafe): `Y1012`.
/// * `Emit::line_width(Some(_))`: wrapping is not implemented; refused
///   with `Y1017` rather than silently ignored.
pub(crate) fn output_rules(settings: &Settings) -> Result<Rules, Error> {
    // Only output conflicts concern the serializer.
    crate::construct::settings::diagnostics(settings, |c| matches!(c, Conflict::ZeroIndent))?;
    let schema = settings.get_schema();
    if schema != Schema::Core {
        let name = if schema == Schema::Json {
            "JSON"
        } else {
            "Failsafe"
        };
        let d = Diagnostic::error(
            code::INCORRECT_SETTINGS,
            format!("output is written for the Core schema; the {name} schema would not read it back as the same values"),
            Label::new(Span::empty(0), ""),
        )
        .with_help("serialize with Schema::Core (the default); every reader-safe output reads back under Core with any YAML 1.1 setting");
        return Err(Error::from_diagnostic(d));
    }
    if let Some(width) = settings.get_emit().get_line_width() {
        let d = Diagnostic::error(
            code::UNSUPPORTED_SETTING,
            format!(
                "line wrapping (Emit::line_width(Some({width}))) is not supported by this version"
            ),
            Label::new(Span::empty(0), ""),
        )
        .with_help("use Emit::line_width(None) (the default): output is never wrapped");
        return Err(Error::from_diagnostic(d));
    }
    Rules::new(settings.get_emit(), settings)
}

/// Where the next node goes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Slot {
    /// The cursor is at `col`, where a collection may start compactly
    /// (after `- `, `? `, `: `, or at a line start). Otherwise the cursor is
    /// after text on the line (`key:`, `---`): a scalar needs a space
    /// before it and a block collection starts on the next line.
    compact: bool,
    /// Column of a block mapping's keys.
    pub(crate) map_col: usize,
    /// Column of a block sequence's `-`.
    pub(crate) seq_col: usize,
    /// Where a literal block's lines go.
    block: BlockIndent,
}

impl Slot {
    /// At the start of a line at column 0 (a root node).
    pub(crate) fn root(rules: &Rules) -> Self {
        Self {
            compact: true,
            map_col: 0,
            seq_col: 0,
            block: root_block(rules),
        }
    }

    /// After an indicator (`- `, `? `, `: `) at column `parent`.
    pub(crate) fn inline(parent: usize, rules: &Rules) -> Self {
        let col = parent.saturating_add(2);
        Self {
            compact: true,
            map_col: col,
            seq_col: col,
            block: BlockIndent {
                content: parent.saturating_add(rules.indent()),
                indicator: Some(rules.indent()),
            },
        }
    }

    /// After `key:` for a key at column `key_col`.
    pub(crate) fn value(key_col: usize, rules: &Rules) -> Self {
        let nested = key_col.saturating_add(rules.indent());
        Self {
            compact: false,
            map_col: nested,
            seq_col: if rules.indent_sequences() {
                nested
            } else {
                key_col
            },
            block: BlockIndent {
                content: nested,
                indicator: Some(rules.indent()),
            },
        }
    }

    /// After `---` (a root node on the marker's line).
    fn after_marker(rules: &Rules) -> Self {
        Self {
            compact: false,
            map_col: 0,
            seq_col: 0,
            block: root_block(rules),
        }
    }
}

/// A root literal block: lines at `indent`, and no indentation indicator.
/// The spec puts a root node's parent at -1 (YAML 1.2.2 §8.1.1.1, so `|3`
/// for lines at 2 spaces) and libyaml-based readers at 0 (`|2`), so no
/// indicator reads the same everywhere (every reader must read it back); a text
/// that needs one is double-quoted instead.
fn root_block(rules: &Rules) -> BlockIndent {
    BlockIndent {
        content: rules.indent(),
        indicator: None,
    }
}

/// The emitter's explicit stack.
pub(crate) enum Task<'a> {
    /// Write a node.
    Node(&'a Value, Slot),
    /// The remaining entries of a block sequence at `col`.
    Seq(std::slice::Iter<'a, Value>, usize),
    /// The remaining entries of a block mapping at `col`.
    Map(MappingIter<'a>, usize),
    /// The `: value` half of an explicit entry at `col`.
    ExplicitValue(&'a Value, usize),
}

/// Starts documents after the first one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// The first document: no marker.
    First,
    /// A later document: `---` first.
    Next,
}

/// Reusable, empty-between-uses stacks for the `Value` walk and the depth
/// walk: the streaming serializer writes many small
/// `Value`s (hand-overs, collection keys) per document, and a fresh stack
/// per `Value` was an allocation each.
///
/// A stack borrows the `Value` it walks, so it is stored at `'static`
/// and lent out at the caller's lifetime (`Vec` is covariant); handing it
/// back empties it and collects it into the `'static` type, which reuses
/// the allocation (same element layout; std's in-place `collect`).
///
/// That reuse is an optimisation std performs, not one it guarantees
/// (`Vec`'s in-place iteration specialisation is unstable and documented
/// as an implementation detail). Were a toolchain to stop doing it, the
/// output would not change — each hand-back would allocate a fresh empty
/// stack instead — and the exact allocation gates would fail
/// (`oracles/tests/ser_allocations.rs`, the hand-over counts): that is
/// the check this relies on.
#[derive(Default)]
pub(crate) struct Stacks {
    tasks: Vec<Task<'static>>,
    walk: Vec<(&'static Value, usize)>,
}

impl Stacks {
    /// The `Value`-walk stack, lent out.
    pub(crate) fn take_tasks<'a>(&mut self) -> Vec<Task<'a>> {
        std::mem::take(&mut self.tasks)
    }

    /// Returns the `Value`-walk stack.
    pub(crate) fn put_tasks(&mut self, tasks: Vec<Task<'_>>) {
        self.tasks = recycle(tasks);
    }

    /// The depth of `v` ([`Value::depth`] stopping past `cap`) on the
    /// reused depth-walk stack.
    pub(crate) fn depth(&mut self, v: &Value, cap: usize) -> usize {
        let mut walk = std::mem::take(&mut self.walk);
        let d = crate::value::walk::depth_with(v, cap, &mut walk);
        self.walk = recycle(walk);
        d
    }
}

/// An empty `Vec<U>` in the allocation of `v` (its elements are dropped):
/// `T` and `U` differ only in lifetimes here, so std's in-place `collect`
/// keeps the buffer.
fn recycle<T, U>(mut v: Vec<T>) -> Vec<U> {
    v.clear();
    v.into_iter().filter_map(|_| None).collect()
}

/// Writes `value` as one document (ending with a line break) to `out`.
///
/// # Errors
///
/// `Y2005` when `value` nests deeper than `limits.max_depth` (nothing is
/// written); `Y3015` for a value no output can represent (`out` then holds
/// a partial document the caller discards).
pub(crate) fn emit_document(
    out: &mut String,
    value: &Value,
    rules: &Rules,
    limits: &Limits,
    start: Start,
) -> Result<(), Error> {
    // Max_depth applies wherever our APIs consume a Value.
    value
        .check_depth(limits)
        .map_err(crate::value::DepthExceeded::into_error)?;
    let slot = document_start(out, rules, start);
    Emitter { out, rules }.run(value, slot, false)?;
    out.push('\n');
    Ok(())
}

/// Writes the start of a document (nothing, or its `---` marker) and
/// returns where its root node goes. Needs no look-ahead at the root, so
/// the streaming serializer starts documents with it too.
///
/// `Layout::SerdeYaml`: libyaml writes a root scalar, a root tag, and an
/// empty root collection on the marker's line (`--- x`, `--- !t`,
/// `--- []`). A non-empty untagged root collection starts on the next
/// line, and from [`Slot::after_marker`] it does: its block opening
/// writes the line break (`---` `\n` `- a`), the same bytes as `---\n`
/// followed by a root slot, so the marker slot serves every root.
pub(crate) fn document_start(out: &mut String, rules: &Rules, start: Start) -> Slot {
    match start {
        Start::First => Slot::root(rules),
        Start::Next if rules.serde_yaml_layout() => {
            out.push_str("---");
            Slot::after_marker(rules)
        }
        Start::Next => {
            out.push_str("---\n");
            Slot::root(rules)
        }
    }
}

fn unrepresentable(what: String) -> Error {
    Error::new(code::UNREPRESENTABLE_VALUE, what, None)
}

/// Null, bool, or number.
pub(crate) fn write_plain_scalar(out: &mut String, rules: &Rules, v: &Value) -> Result<(), Error> {
    match v {
        Value::Null => scalar::write_null(out),
        Value::Bool(b) => scalar::write_bool(out, *b),
        Value::Number(n) => rules.write_number(out, n)?,
        // Callers pass only scalars.
        _ => {
            debug_assert!(false, "write_plain_scalar given a non-scalar");
            return Err(Error::internal(
                "write_plain_scalar given a non-scalar",
                None,
            ));
        }
    }
    Ok(())
}

/// Writes nodes into `out` under `rules`: the `Value` walk ([`Emitter::run`])
/// and the primitives the streaming serializer shares with it.
pub(crate) struct Emitter<'o, 'r> {
    out: &'o mut String,
    rules: &'r Rules,
}

impl<'o, 'r> Emitter<'o, 'r> {
    /// An emitter appending to `out`.
    pub(crate) fn new(out: &'o mut String, rules: &'r Rules) -> Self {
        Self { out, rules }
    }
}

impl<'a> Emitter<'_, '_> {
    /// Writes `root` at `slot`, iteratively. `tagged`: a tag was
    /// already written for it (the streaming serializer's enum variants),
    /// so `root` is that tag's content.
    pub(crate) fn run(&mut self, root: &'a Value, slot: Slot, tagged: bool) -> Result<(), Error> {
        self.run_with(root, slot, tagged, &mut Vec::new())
    }

    /// [`Emitter::run`] on a caller's stack (the streaming
    /// serializer reuses one per document; see [`Stacks`]). The stack is
    /// emptied first and left empty when the walk completes.
    pub(crate) fn run_with(
        &mut self,
        root: &'a Value,
        slot: Slot,
        tagged: bool,
        stack: &mut Vec<Task<'a>>,
    ) -> Result<(), Error> {
        stack.clear();
        if tagged {
            self.content(root, slot, true, stack)?;
        } else {
            stack.push(Task::Node(root, slot));
        }
        while let Some(task) = stack.pop() {
            match task {
                Task::Node(v, slot) => self.node(v, slot, stack)?,
                Task::Seq(mut it, col) => {
                    if let Some(item) = it.next() {
                        self.newline(col);
                        self.out.push_str("- ");
                        stack.push(Task::Seq(it, col));
                        stack.push(Task::Node(item, Slot::inline(col, self.rules)));
                    }
                }
                Task::Map(mut it, col) => {
                    if let Some((k, v)) = it.next() {
                        self.newline(col);
                        stack.push(Task::Map(it, col));
                        self.entry(k, v, col, stack)?;
                    }
                }
                Task::ExplicitValue(v, col) => {
                    self.newline(col);
                    self.out.push_str(": ");
                    stack.push(Task::Node(v, Slot::inline(col, self.rules)));
                }
            }
        }
        Ok(())
    }

    /// Starts a new line at column `col`.
    pub(crate) fn newline(&mut self, col: usize) {
        self.out.push('\n');
        scalar::push_spaces(self.out, col);
    }

    /// Writes the tag property of a node at `slot` (its content follows
    /// with `tagged` set).
    fn tag(&mut self, slot: Slot, tag: TagStr<'_>) -> Result<(), Error> {
        if !self.rules.tag_fits(tag.len()) {
            return Err(self.rules.tag_too_long(tag.len()));
        }
        if !slot.compact {
            self.out.push(' ');
        }
        scalar::write_tag(self.out, tag).map_err(|()| {
            unrepresentable(format!(
                "the tag `{tag}` has no spec-valid form that reads back as the same tag"
            ))
        })
    }

    /// Writes the local tag `!name` of a node at `slot`: [`Emitter::tag`]
    /// for `format!("!{name}")`, without building it.
    ///
    /// # Errors
    ///
    /// `Internal` for an empty `name` (callers pass only non-empty names;
    /// never a panic), writing nothing.
    pub(crate) fn local_tag(&mut self, slot: Slot, name: &str) -> Result<(), Error> {
        if name.is_empty() {
            debug_assert!(false, "an empty local tag name");
            return Err(Error::internal("an empty local tag name", None));
        }
        let len = name.len().saturating_add(1);
        if !self.rules.tag_fits(len) {
            return Err(self.rules.tag_too_long(len));
        }
        if !slot.compact {
            self.out.push(' ');
        }
        scalar::write_local_tag(self.out, name)
            .map_err(|()| Error::internal("an empty local tag name", None))
    }

    /// Writes a string node.
    pub(crate) fn string(&mut self, slot: Slot, tagged: bool, s: &str) {
        self.separate(slot, tagged);
        let style = self.rules.string_style(s, slot.block);
        self.rules.write_string(self.out, s, style, slot.block);
    }

    /// Writes the key of a block mapping entry at `col` whose key is the
    /// string `name`: implicit (`name:`) when it can be, else the explicit
    /// `? name` / `: ` form; returns where the entry's value goes. What
    /// [`Emitter::entry`] writes for a `Value::String` key.
    pub(crate) fn str_key(&mut self, name: &str, col: usize) -> Slot {
        if self.rules.write_implicit_key(self.out, name, None) {
            self.out.push(':');
            return Slot::value(col, self.rules);
        }
        self.out.push_str("? ");
        self.string(Slot::inline(col, self.rules), false, name);
        self.explicit_value(col)
    }

    /// Writes the key `k` of a block mapping entry at `col` (what
    /// [`Emitter::entry`] writes for it) and returns where the value goes.
    /// An explicit key is written with [`Emitter::run`], iteratively.
    ///
    /// # Errors
    ///
    /// `Y3015` for a key no output can represent; the entry is then
    /// written in part (the caller discards the document).
    pub(crate) fn key(
        &mut self,
        k: &'a Value,
        col: usize,
        stack: &mut Vec<Task<'a>>,
    ) -> Result<Slot, Error> {
        if self.write_implicit_key(k)? {
            self.out.push(':');
            return Ok(Slot::value(col, self.rules));
        }
        self.out.push_str("? ");
        self.run_with(k, Slot::inline(col, self.rules), false, stack)?;
        Ok(self.explicit_value(col))
    }

    /// The `: ` line of an explicit entry at `col`.
    fn explicit_value(&mut self, col: usize) -> Slot {
        self.newline(col);
        self.out.push_str(": ");
        Slot::inline(col, self.rules)
    }

    /// Writes a node (a collection's first entry; the rest are queued).
    fn node(&mut self, v: &'a Value, slot: Slot, stack: &mut Vec<Task<'a>>) -> Result<(), Error> {
        let (tag, content) = match v {
            Value::Tagged(t) => (Some(t.tag.parts()), &t.value),
            other => (None, other),
        };
        if let Some(tag) = tag {
            self.tag(slot, tag)?;
            match content {
                Value::Tagged(_) => {
                    return Err(unrepresentable(
                        "a tagged value inside a tagged value: a YAML node has one tag".to_owned(),
                    ));
                }
                Value::String(_) | Value::Sequence(_) | Value::Mapping(_) => {}
                _ => {
                    return Err(unrepresentable(format!(
                        "a `{tag}`-tagged non-string scalar: a custom-tagged scalar always reads back as a string"
                    )));
                }
            }
        }
        self.content(content, slot, tag.is_some(), stack)
    }

    /// Writes an untagged node, or the content of a node whose tag
    /// (`tagged`) is written.
    fn content(
        &mut self,
        content: &'a Value,
        slot: Slot,
        tagged: bool,
        stack: &mut Vec<Task<'a>>,
    ) -> Result<(), Error> {
        match content {
            Value::Sequence(s) => match s.as_slice().split_first() {
                None => self.inline_text(slot, tagged, "[]"),
                Some((first, rest)) => {
                    let col = slot.seq_col;
                    self.open_block(slot, tagged, col);
                    self.out.push_str("- ");
                    stack.push(Task::Seq(rest.iter(), col));
                    stack.push(Task::Node(first, Slot::inline(col, self.rules)));
                }
            },
            Value::Mapping(m) => {
                let mut it = m.iter();
                match it.next() {
                    None => self.inline_text(slot, tagged, "{}"),
                    Some((k, val)) => {
                        let col = slot.map_col;
                        self.open_block(slot, tagged, col);
                        stack.push(Task::Map(it, col));
                        self.entry(k, val, col, stack)?;
                    }
                }
            }
            Value::String(s) => self.string(slot, tagged, s),
            other => {
                self.separate(slot, tagged);
                write_plain_scalar(self.out, self.rules, other)?;
            }
        }
        Ok(())
    }

    /// A space before a scalar written after text on the line.
    pub(crate) fn separate(&mut self, slot: Slot, tagged: bool) {
        if tagged || !slot.compact {
            self.out.push(' ');
        }
    }

    /// Writes a one-token node (`[]`, `{}`).
    pub(crate) fn inline_text(&mut self, slot: Slot, tagged: bool, text: &str) {
        self.separate(slot, tagged);
        self.out.push_str(text);
    }

    /// Positions the cursor for a block collection's first entry at `col`.
    pub(crate) fn open_block(&mut self, slot: Slot, tagged: bool, col: usize) {
        if tagged || !slot.compact {
            self.newline(col);
        }
    }

    /// Writes a mapping entry whose key starts at the cursor (column `col`).
    fn entry(
        &mut self,
        k: &'a Value,
        v: &'a Value,
        col: usize,
        stack: &mut Vec<Task<'a>>,
    ) -> Result<(), Error> {
        if self.write_implicit_key(k)? {
            self.out.push(':');
            stack.push(Task::Node(v, Slot::value(col, self.rules)));
        } else {
            self.out.push_str("? ");
            stack.push(Task::ExplicitValue(v, col));
            stack.push(Task::Node(k, Slot::inline(col, self.rules)));
        }
        Ok(())
    }

    /// Writes plain text (an indicator) at the cursor.
    pub(crate) fn push_str(&mut self, s: &str) {
        self.out.push_str(s);
    }

    /// Writes a null, bool, or number node.
    pub(crate) fn plain(&mut self, slot: Slot, tagged: bool, v: &Value) -> Result<(), Error> {
        self.separate(slot, tagged);
        write_plain_scalar(self.out, self.rules, v)
    }

    /// Writes `k` as a one-line implicit key at the cursor and returns
    /// `true`, or writes nothing and returns `false` for the `? ` form.
    ///
    /// Keys: a scalar key of any type is implicit (`1: x`,
    /// `true: x`, `null: x`, `'yes': x`) unless it is too long or a
    /// multi-line string; only collection keys use `? `. The key is
    /// written in place and measured, never built in a buffer first.
    fn write_implicit_key(&mut self, k: &Value) -> Result<bool, Error> {
        let (tag, content) = match k {
            Value::Tagged(t) => (Some(t.tag.parts()), &t.value),
            other => (None, other),
        };
        let empty = match content {
            // Reported when written as a node.
            Value::Tagged(_) => return Ok(false),
            Value::String(s) => return Ok(self.rules.write_implicit_key(self.out, s, tag)),
            Value::Sequence(s) if s.is_empty() && self.rules.serde_yaml_layout() => Some("[]"),
            Value::Mapping(m) if m.is_empty() && self.rules.serde_yaml_layout() => Some("{}"),
            Value::Sequence(_) | Value::Mapping(_) => return Ok(false),
            // A tagged non-string scalar is an error, reported as a node.
            _ if tag.is_some() => return Ok(false),
            _ => None,
        };
        let start = self.out.len();
        if let Some(tag) = tag {
            if !self.rules.tag_fits(tag.len()) || scalar::write_tag(self.out, tag).is_err() {
                self.out.truncate(start);
                return Ok(false);
            }
            self.out.push(' ');
        }
        match empty {
            Some(text) => self.out.push_str(text),
            None => write_plain_scalar(self.out, self.rules, content)?,
        }
        let fits = self.rules.short_key(self.out.len().saturating_sub(start));
        if !fits {
            self.out.truncate(start);
        }
        Ok(fits)
    }
}

/// The style a string takes as a root node (for tests).
#[cfg(test)]
pub(crate) fn root_style(rules: &Rules, s: &str) -> scalar::Style {
    rules.string_style(s, root_block(rules))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::value::{Mapping, Tag, TaggedValue};
    use oneyaml_core::{Emit, Layout};

    fn emit(v: &Value, settings: &Settings) -> String {
        let rules = output_rules(settings).unwrap();
        let mut out = String::new();
        emit_document(&mut out, v, &rules, settings.get_limits(), Start::First).unwrap();
        out
    }

    #[test]
    fn no_indentation_indicator_at_the_root() {
        let rules = output_rules(&Settings::new()).unwrap();
        assert_eq!(root_style(&rules, " a\nb"), scalar::Style::Double);
        assert_eq!(root_style(&rules, "a\n b"), scalar::Style::Literal);
        assert_eq!(
            emit(&Value::from(" a\nb"), &Settings::new()),
            "\" a\\nb\"\n"
        );
        let nested = Value::from(vec![Value::from(" a\nb")]);
        assert_eq!(emit(&nested, &Settings::new()), "- |2-\n   a\n  b\n");
    }

    #[test]
    fn tagged_non_string_scalars_are_refused() {
        let v = Value::from(TaggedValue {
            tag: Tag::new("!n"),
            value: Value::from(1),
        });
        let rules = output_rules(&Settings::new()).unwrap();
        let e = emit_document(&mut String::new(), &v, &rules, &Limits::new(), Start::First)
            .unwrap_err();
        assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE);
        let mut m = Mapping::new();
        m.insert(v, Value::Null);
        let e = emit_document(
            &mut String::new(),
            &Value::from(m),
            &rules,
            &Limits::new(),
            Start::First,
        )
        .unwrap_err();
        assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE);
    }

    #[test]
    fn serde_yaml_layout_marks() {
        let s = Settings::new().emit(Emit::new().layout(Layout::SerdeYaml));
        let rules = output_rules(&s).unwrap();
        let mut out = String::new();
        emit_document(
            &mut out,
            &Value::from(1),
            &rules,
            &Limits::new(),
            Start::First,
        )
        .unwrap();
        emit_document(
            &mut out,
            &Value::from("x"),
            &rules,
            &Limits::new(),
            Start::Next,
        )
        .unwrap();
        emit_document(
            &mut out,
            &Value::from(vec![Value::from(1)]),
            &rules,
            &Limits::new(),
            Start::Next,
        )
        .unwrap();
        assert_eq!(out, "1\n--- x\n---\n- 1\n");
    }
}

#[cfg(test)]
mod recycle_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn recycling_keeps_the_allocation() {
        let v = Value::from(vec![Value::from(1), Value::from(vec![Value::Null])]);
        let mut st = Stacks::default();
        assert_eq!(st.depth(&v, 10), 2);
        let cap = st.walk.capacity();
        let ptr = st.walk.as_ptr() as usize;
        assert!(cap > 0);
        assert_eq!(st.depth(&v, 10), 2);
        assert_eq!(st.walk.capacity(), cap);
        assert_eq!(st.walk.as_ptr() as usize, ptr);
        let mut t = st.take_tasks();
        let rules = output_rules(&Settings::new()).unwrap();
        let mut out = String::new();
        Emitter::new(&mut out, &rules)
            .run_with(&v, Slot::root(&rules), false, &mut t)
            .unwrap();
        let cap = t.capacity();
        st.put_tasks(t);
        assert!(cap > 0);
        assert_eq!(st.tasks.capacity(), cap);
    }
}
