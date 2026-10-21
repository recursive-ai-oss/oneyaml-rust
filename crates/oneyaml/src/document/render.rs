//! Writing new nodes for the edit layer.
//!
//! Block text comes from the serializer's own emitter (`crate::emit`), so
//! a new node is laid out exactly as `to_string` lays it out, with the
//! every-reader-safe scalar rules: the value is written as the one
//! entry (or item) of a root collection and the lines after the first are
//! shifted to the target column. YAML block indentation is relative, so a
//! uniform shift keeps its meaning (a literal block's indentation
//! indicator counts from its parent's indentation, which moves with it).
//!
//! Multi-line strings are `|` block scalars wherever
//! the context permits"); where it does not (the syntax layer refuses the
//! splice), the edit layer asks again with [`Render::one_line`], which
//! writes them double-quoted in flow style.
//!
//! Flow text (for flow collections) is written here, iteratively, with
//! the same scalar rules; strings that plain flow text cannot hold are
//! quoted.
//!
//! Line breaks (2026-09-28): new text uses the document's
//! own break, and re-indenting treats CR, LF and CRLF alike (YAML 1.2.2
//! section 5.4 `b-break`), keeping each break as written.

use oneyaml_core::{Emit, Limits, QuoteStyle, Settings, code};
use oneyaml_syntax::event::Chomping;

use super::{EditError, ScalarStyle};
use crate::emit::scalar::{self, BlockIndent, Rules, Style};
use crate::emit::{Start, emit_document, output_rules};
use crate::value::{Mapping, MappingIter, Value};

/// The output rules for one edit: the document's settings, with the
/// indentation and sequence placement inferred from the context.
pub(super) struct Render {
    rules: Rules,
    limits: Limits,
    double: bool,
    /// The document's line break.
    nl: &'static str,
    /// Multi-line strings as one-line flow text (the fallback).
    one_line: bool,
}

/// How the block scalar being replaced is written: what a new block scalar
/// in its place keeps.
pub(super) struct BlockStyle {
    /// Folded (`>`), else literal (`|`).
    pub(super) folded: bool,
    pub(super) chomping: Chomping,
    /// The indentation indicator, if the old header had one.
    pub(super) indicator: Option<u8>,
    /// Column of the old content lines.
    pub(super) content: usize,
    /// Column of the key or dash that holds the scalar.
    pub(super) parent: usize,
}

/// The flow writer's explicit stack.
enum Task<'a> {
    Node(&'a Value),
    Key(&'a Value),
    Text(&'static str),
    Seq(std::slice::Iter<'a, Value>, bool),
    Map(MappingIter<'a>, bool),
}

fn unrepresentable(what: &str) -> EditError {
    EditError::rejected(&crate::Error::new(
        code::UNREPRESENTABLE_VALUE,
        what.to_owned(),
        None,
    ))
}

/// Longest implicit key (YAML 1.2.2 §7.4.2).
const MAX_IMPLICIT_KEY: usize = 1024;

impl Render {
    /// `emit` is the inferred layout; `settings` are checked for
    /// output (`Y1012`, `Y1017`) as the serializer checks them.
    pub(super) fn new(
        settings: &Settings,
        emit: &Emit,
        nl: &'static str,
    ) -> Result<Self, EditError> {
        output_rules(settings).map_err(|e| EditError::rejected(&e))?;
        let rules = Rules::new(emit, settings).map_err(|e| EditError::rejected(&e))?;
        Ok(Self {
            rules,
            limits: settings.get_limits().clone(),
            double: emit.get_quote_style() == QuoteStyle::Double,
            nl,
            one_line: false,
        })
    }

    /// The renderer for a string that replaces a node: plain unless the
    /// document's schema reads the text as another type.
    pub(super) fn schema_floor(mut self) -> Self {
        self.rules = self.rules.for_edit();
        self
    }

    /// The fallback renderer: multi-line strings as one-line flow text.
    pub(super) fn one_line(mut self) -> Self {
        self.one_line = true;
        self
    }

    /// Whether `v` is written as one-line flow text.
    fn flat(&self, v: &Value) -> bool {
        self.one_line && multiline(v)
    }

    /// `v` as a root node, without the final line break.
    pub(super) fn root(&self, v: &Value) -> Result<String, EditError> {
        if self.flat(v) {
            return self.flow(v);
        }
        self.emit_root(v)
    }

    /// The emitter's text for `v` as a root document, without the final
    /// line break — except when that break is the last line of a keep
    /// (`|+`) block scalar's content (the text then ends with an empty
    /// line, `|+⏎  x⏎⏎`): it is part of the value, and callers add no
    /// other break to text that ends its line.
    fn emit_root(&self, v: &Value) -> Result<String, EditError> {
        let mut out = String::new();
        emit_document(&mut out, v, &self.rules, &self.limits, Start::First)
            .map_err(|e| EditError::rejected(&e))?;
        // Fuzz finding E7: popping that break wrote
        // `"x\n\n"` as `|+⏎  x⏎`, which reads `"x\n"`. The emitter writes
        // an empty last line only for a keep scalar's trailing breaks.
        if out.ends_with('\n') && !out.ends_with("\n\n") {
            out.pop();
        }
        if self.nl != "\n" {
            // The emitter writes LF; every LF it writes is a line break
            // (an escaped one is written `\n`, two characters).
            out = out.replace('\n', self.nl);
        }
        Ok(out)
    }

    /// The text after `key:` for a value whose entry starts at `col`
    /// (begins with a space or a line break).
    pub(super) fn after_key(&self, v: &Value, col: usize) -> Result<String, EditError> {
        if self.flat(v) {
            return Ok(format!(" {}", self.flow(v)?));
        }
        let mut m = Mapping::new();
        m.insert(Value::from("x"), v.clone());
        let text = self.emit_root(&Value::Mapping(m))?;
        let rest = text.strip_prefix("x:").ok_or(EditError::Internal)?;
        Ok(shift(rest, col))
    }

    /// The text after `-` for an item whose `-` is at `col`.
    pub(super) fn after_dash(&self, v: &Value, col: usize) -> Result<String, EditError> {
        if self.flat(v) {
            return Ok(format!(" {}", self.flow(v)?));
        }
        let text = self.emit_root(&Value::Sequence(vec![v.clone()].into()))?;
        let rest = text.strip_prefix('-').ok_or(EditError::Internal)?;
        Ok(shift(rest, col))
    }

    /// A block mapping entry `key: value` at `col` (first line unindented).
    pub(super) fn entry(&self, key: &Value, v: &Value, col: usize) -> Result<String, EditError> {
        if self.flat(key) || self.flat(v) {
            return self.flow_entry(key, v);
        }
        let mut m = Mapping::new();
        m.insert(key.clone(), v.clone());
        Ok(shift(&self.emit_root(&Value::Mapping(m))?, col))
    }

    /// A block sequence item `- value` at `col` (first line unindented).
    pub(super) fn item(&self, v: &Value, col: usize) -> Result<String, EditError> {
        if self.flat(v) {
            return Ok(format!("- {}", self.flow(v)?));
        }
        let text = self.emit_root(&Value::Sequence(vec![v.clone()].into()))?;
        Ok(shift(&text, col))
    }

    /// `v` on its own line at `col` (first line unindented).
    pub(super) fn own(&self, v: &Value, col: usize) -> Result<String, EditError> {
        Ok(shift(&self.root(v)?, col))
    }

    /// A flow mapping entry `key: value`.
    pub(super) fn flow_entry(&self, key: &Value, v: &Value) -> Result<String, EditError> {
        let mut m = Mapping::new();
        m.insert(key.clone(), v.clone());
        let text = self.flow(&Value::Mapping(m))?;
        let inner = text
            .strip_prefix('{')
            .and_then(|t| t.strip_suffix('}'))
            .ok_or(EditError::Internal)?;
        Ok(inner.to_owned())
    }

    /// A key as an implicit key (one line), in block or flow context.
    pub(super) fn key(&self, key: &str, flow: bool) -> Result<String, EditError> {
        let mut out = String::new();
        let ok = if flow {
            self.flow_string(&mut out, key);
            out.len() <= MAX_IMPLICIT_KEY && !out.contains('\n')
        } else {
            self.rules.write_implicit_key(&mut out, key, None)
        };
        if ok {
            Ok(out)
        } else {
            Err(EditError::Unsupported {
                at: oneyaml_core::Path::new(),
                reason: "a key too long or multi-line for an implicit key",
            })
        }
    }

    /// `text` written in the scalar style `style` when that style holds it
    /// exactly and the every-reader-safe rule allows it (style inference:
    /// keep how the document wrote the scalar); `None` otherwise.
    pub(super) fn keep_style(&self, text: &str, style: ScalarStyle) -> Option<String> {
        let block = BlockIndent {
            content: 0,
            indicator: None,
        };
        let chosen = self.rules.string_style(text, block);
        let st = match (style, chosen) {
            (
                ScalarStyle::DoubleQuoted,
                Style::Plain | Style::Single | Style::Double | Style::Literal,
            ) => Style::Double,
            (ScalarStyle::SingleQuoted, Style::Plain | Style::Single) => Style::Single,
            // A line break is written as the escape, on the line of the value it replaces.
            (ScalarStyle::SingleQuoted | ScalarStyle::Plain, Style::Literal) => Style::Double,
            (ScalarStyle::Plain, Style::Plain) if !text.contains([',', '[', ']', '{', '}']) => {
                Style::Plain
            }
            _ => return None,
        };
        let mut out = String::new();
        self.rules.write_string(&mut out, text, st, block);
        Some(out)
    }

    /// `text` as a block scalar in the style of the one it replaces: the text
    /// after `key:` (or `-`) without its leading space, the content lines at
    /// the old content's indentation `content`. `parent` is the column of the
    /// key (or dash), which an indentation indicator counts from.
    ///
    /// The chomping indicator changes only as far as the value needs: strip
    /// holds no final line break, clip exactly one, keep one or more. Folded
    /// text writes a line break between lines of text as an empty line,
    /// because a single break folds to a space.
    ///
    /// `None` when a block scalar cannot hold the text (it needs escapes, or
    /// has no content); the caller then writes it as any other string.
    pub(super) fn block_scalar(&self, text: &str, old: &BlockStyle) -> Option<String> {
        let content = text.trim_end_matches('\n');
        if content.is_empty() || !scalar::block_text_ok(text) || self.one_line {
            return None;
        }
        let breaks = text.len().saturating_sub(content.len());
        let chomp = match (old.chomping, breaks) {
            (Chomping::Strip, 0) | (Chomping::Clip, 1) | (Chomping::Keep, 1..) => old.chomping,
            (_, 0) => Chomping::Strip,
            (_, 1) => Chomping::Clip,
            _ => Chomping::Keep,
        };
        let indent = old.content.checked_sub(old.parent).filter(|d| *d > 0)?;
        let spaced = |l: &str| l.starts_with([' ', '\t']);
        let first = content.split('\n').find(|l| !l.is_empty())?;
        let needs = first.starts_with(' ');
        let digit = match old.indicator {
            Some(d) => Some(d),
            None if needs => Some(u8::try_from(indent).ok()?),
            None => None,
        };
        if digit.is_some_and(|d| !(1..=9).contains(&d) || usize::from(d) != indent) {
            return None;
        }
        let mut lines: Vec<&str> = Vec::new();
        if old.folded {
            // Between two lines of text a break needs as many empty lines as
            // it stands for; next to a more-indented line one break is kept
            // as it is, so one fewer.
            let mut prev: Option<&str> = None;
            let mut empties = 0usize;
            for l in content.split('\n') {
                if l.is_empty() {
                    empties = empties.saturating_add(1);
                    if prev.is_none() {
                        lines.push(l);
                    }
                    continue;
                }
                if let Some(p) = prev {
                    let m = empties.saturating_add(1);
                    let extra = if spaced(p) || spaced(l) {
                        m.saturating_sub(1)
                    } else {
                        m
                    };
                    lines.extend(std::iter::repeat_n("", extra));
                }
                lines.push(l);
                prev = Some(l);
                empties = 0;
            }
        } else {
            lines.extend(content.split('\n'));
        }
        // The text is the lines times the indentation each: more than the
        // document may hold is never built (a long text under a deeply
        // indented block would multiply), the caller writes it another way.
        let nl = self.nl.len();
        let mut size = 3usize.checked_add(breaks.checked_mul(nl)?)?;
        for l in &lines {
            size = size.checked_add(nl)?;
            if !l.is_empty() {
                size = size.checked_add(old.content)?.checked_add(l.len())?;
            }
        }
        if size > self.limits.get_max_document_size() {
            return None;
        }
        let mut out = String::with_capacity(size);
        out.push(if old.folded { '>' } else { '|' });
        if let Some(d) = digit {
            out.push(char::from(b'0'.saturating_add(d)));
        }
        match chomp {
            Chomping::Strip => out.push('-'),
            Chomping::Keep => out.push('+'),
            _ => {}
        }
        for l in lines {
            out.push_str(self.nl);
            if !l.is_empty() {
                out.extend(std::iter::repeat_n(' ', old.content));
                out.push_str(l);
            }
        }
        if chomp == Chomping::Keep {
            for _ in 0..breaks {
                out.push_str(self.nl);
            }
        }
        Some(out)
    }

    /// `v` as one line of flow text, with an explicit stack (no recursion),
    /// bounded by `max_depth` first.
    pub(super) fn flow(&self, v: &Value) -> Result<String, EditError> {
        v.check_depth(&self.limits)
            .map_err(|e| EditError::rejected(&e.into_error()))?;
        let mut out = String::new();
        let mut stack = vec![Task::Node(v)];
        while let Some(t) = stack.pop() {
            match t {
                Task::Text(s) => out.push_str(s),
                Task::Node(v) => self.flow_node(&mut out, v, &mut stack)?,
                Task::Key(k) => {
                    let scalar = !matches!(k.untag_ref(), Value::Sequence(_) | Value::Mapping(_));
                    if scalar {
                        let start = out.len();
                        self.flow_node(&mut out, k, &mut stack)?;
                        if out.len().saturating_sub(start) > MAX_IMPLICIT_KEY {
                            out.insert_str(start, "? ");
                        }
                    } else {
                        // A collection key: explicit, as the emitter does.
                        out.push_str("? ");
                        self.flow_node(&mut out, k, &mut stack)?;
                    }
                }
                Task::Seq(mut it, first) => match it.next() {
                    Some(x) => {
                        if !first {
                            out.push_str(", ");
                        }
                        stack.push(Task::Seq(it, false));
                        stack.push(Task::Node(x));
                    }
                    None => out.push(']'),
                },
                Task::Map(mut it, first) => match it.next() {
                    Some((k, x)) => {
                        if !first {
                            out.push_str(", ");
                        }
                        stack.push(Task::Map(it, false));
                        stack.push(Task::Node(x));
                        stack.push(Task::Text(": "));
                        stack.push(Task::Key(k));
                    }
                    None => out.push('}'),
                },
            }
        }
        Ok(out)
    }

    fn flow_node<'a>(
        &self,
        out: &mut String,
        v: &'a Value,
        stack: &mut Vec<Task<'a>>,
    ) -> Result<(), EditError> {
        let (tag, content) = match v {
            Value::Tagged(t) => (Some(t.tag.parts()), &t.value),
            other => (None, other),
        };
        if let Some(tag) = tag {
            if scalar::write_tag(out, tag).is_err() {
                return Err(unrepresentable(
                    "a tag with no spec-valid form that reads back as the same tag",
                ));
            }
            out.push(' ');
            match content {
                Value::String(_) | Value::Sequence(_) | Value::Mapping(_) => {}
                Value::Tagged(_) => {
                    return Err(unrepresentable(
                        "a tagged value inside a tagged value: a YAML node has one tag",
                    ));
                }
                _ => {
                    return Err(unrepresentable(
                        "a custom-tagged non-string scalar: it always reads back as a string",
                    ));
                }
            }
        }
        match content {
            Value::Null => scalar::write_null(out),
            Value::Bool(b) => scalar::write_bool(out, *b),
            Value::Number(n) => self
                .rules
                .write_number(out, n)
                .map_err(|e| EditError::rejected(&e))?,
            Value::String(s) => self.flow_string(out, s),
            Value::Sequence(s) => {
                if s.is_empty() {
                    out.push_str("[]");
                } else {
                    out.push('[');
                    stack.push(Task::Seq(s.iter(), true));
                }
            }
            Value::Mapping(m) => {
                if m.is_empty() {
                    out.push_str("{}");
                } else {
                    out.push('{');
                    stack.push(Task::Map(m.iter(), true));
                }
            }
            Value::Tagged(_) => return Err(EditError::Internal),
        }
        Ok(())
    }

    /// A string in flow context: the every-reader-safe choice, with plain
    /// text that holds a flow indicator quoted and literal blocks written
    /// double-quoted (one line).
    fn flow_string(&self, out: &mut String, s: &str) {
        let block = BlockIndent {
            content: 0,
            indicator: None,
        };
        let style = match self.rules.string_style(s, block) {
            Style::Literal => Style::Double,
            Style::Plain if s.contains([',', '[', ']', '{', '}']) => {
                if self.double {
                    Style::Double
                } else {
                    Style::Single
                }
            }
            other => other,
        };
        self.rules.write_string(out, s, style, block);
    }
}

/// Whether `v` holds a multi-line string, which the emitter writes as a
/// literal block. A loop, not recursion.
pub(super) fn multiline(v: &Value) -> bool {
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::String(s) => {
                if s.contains('\n') {
                    return true;
                }
            }
            Value::Sequence(s) => stack.extend(s.iter()),
            Value::Mapping(m) => {
                for (k, x) in m.iter() {
                    stack.push(k);
                    stack.push(x);
                }
            }
            Value::Tagged(t) => stack.push(&t.value),
            _ => {}
        }
    }
    false
}

/// The lines of `text` with the break that ends each (`""` for the last):
/// CR, LF and CRLF alike (section 5.4 `b-break`).
fn lines(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        match rest.find(['\n', '\r']) {
            None => {
                out.push((rest, ""));
                return out;
            }
            Some(i) => {
                let len = if rest.get(i..).is_some_and(|t| t.starts_with("\r\n")) {
                    2
                } else {
                    1
                };
                let end = i.saturating_add(len);
                out.push((rest.get(..i).unwrap_or(""), rest.get(i..end).unwrap_or("")));
                rest = rest.get(end..).unwrap_or("");
            }
        }
    }
}

/// Shifts every non-empty line after the first right by `by` spaces.
pub(super) fn shift(text: &str, by: usize) -> String {
    if by == 0 || !text.contains(['\n', '\r']) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    for (i, (line, brk)) in lines(text).into_iter().enumerate() {
        if i > 0 && !line.is_empty() {
            out.extend(std::iter::repeat_n(' ', by));
        }
        out.push_str(line);
        out.push_str(brk);
    }
    out
}

/// Moves the lines after the first from column `from` to column `to`;
/// comment and blank lines with too little indentation are moved to the
/// new column. `None` when a content line would have to move left past
/// its own indentation.
pub(super) fn reindent(text: &str, from: usize, to: usize) -> Option<String> {
    if from <= to {
        return Some(shift(text, to.saturating_sub(from)));
    }
    let by = from.saturating_sub(to);
    let mut out = String::with_capacity(text.len());
    for (i, (line, brk)) in lines(text).into_iter().enumerate() {
        if i == 0 {
            out.push_str(line);
            out.push_str(brk);
            continue;
        }
        let t = line.trim_start_matches(' ');
        let lead = line.len().saturating_sub(t.len());
        if lead >= by {
            out.push_str(line.get(by..)?);
        } else {
            if !(t.is_empty() || t.starts_with('#')) {
                return None;
            }
            if !t.is_empty() {
                out.extend(std::iter::repeat_n(' ', to));
                out.push_str(t);
            }
        }
        out.push_str(brk);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn shifting_and_reindenting() {
        assert_eq!(shift("a:\n  b: 1\n\n  c", 2), "a:\n    b: 1\n\n    c");
        assert_eq!(shift("a", 4), "a");
        assert_eq!(reindent("a:\n    b", 2, 0).unwrap(), "a:\n  b");
        assert_eq!(reindent("a:\n# c\n    b", 2, 0).unwrap(), "a:\n# c\n  b");
        assert_eq!(reindent("a:\n b", 4, 0), None);
        assert_eq!(reindent("a:\n b", 0, 2).unwrap(), "a:\n   b");
        // Section 5.4: CR and CRLF are line breaks too, each kept as written.
        assert_eq!(shift("a:\r  b\r\n  c", 2), "a:\r    b\r\n    c");
        assert_eq!(reindent("b:\r  c: 1", 2, 0).unwrap(), "b:\rc: 1");
        assert_eq!(reindent("b:\r c: 1", 4, 0), None);
    }

    #[test]
    fn flow_text() {
        let r = Render::new(&Settings::new(), &Emit::new(), "\n").unwrap();
        let v: Value =
            crate::from_str("{a: [1, 'x,y', null], b: {}, 'c d': [], e: \"l1\\nl2\"}").unwrap();
        assert_eq!(
            r.flow(&v).unwrap(),
            "{a: [1, 'x,y', null], b: {}, c d: [], e: \"l1\\nl2\"}"
        );
        let k: Value = crate::from_str("? [1]\n: 2").unwrap();
        assert_eq!(r.flow(&k).unwrap(), "{? [1]: 2}");
        let t: Value = crate::from_str("!t {a: !u x}").unwrap();
        assert_eq!(r.flow(&t).unwrap(), "!t {a: !u x}");
        let bad = Value::Tagged(Box::new(crate::TaggedValue {
            tag: crate::Tag::new("!t"),
            value: Value::from(1),
        }));
        assert!(matches!(r.flow(&bad), Err(EditError::Rejected { .. })));
    }
}
