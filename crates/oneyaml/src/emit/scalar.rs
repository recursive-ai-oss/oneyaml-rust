//! Scalar presentation: the **one** place that decides how a
//! scalar's text is written — plain, single-quoted, double-quoted, or a
//! literal block — and writes it.
//!
//! The interface is deliberately narrow and independent of serde, so the
//! `Document` edit API can format new nodes with it too:
//!
//! * [`Rules::new`] from the output settings ([`Emit`]) and the settings the
//!   output is read back with;
//! * [`Rules::string_style`] picks a [`Style`] for a string at a
//!   [`BlockIndent`] (where a literal block's lines would go);
//! * [`Rules::write_string`] writes it in that style;
//! * [`Rules::write_implicit_key`] writes the one-line form of a string
//!   (with an optional tag) when it can be an implicit mapping key;
//! * [`Rules::write_number`], [`write_bool`], [`write_null`], and
//!   [`write_tag`] write the other scalars and tags.
//!
//! # The every-reader-safe rule
//!
//! A string is written plain only if no reader could take it for anything
//! else: the **one sealed resolver** must resolve the plain text to
//! a string under the Core schema, under the JSON schema (where "no match"
//! is not a non-string), and under Core with every YAML 1.1 behaviour on;
//! it must not be a YAML 1.1 type our resolver does not model, nor a
//! non-string for the other readers the every-reader rule names — Ruby Psych
//! and Go `yaml.v2`/`v3` ([`foreign_yaml11`]: timestamps, `<<`, `=`, 1.1
//! float spellings, comma-grouped numbers, base 60 led by any digit,
//! `.iNf`, `_` anywhere in a number); and it must have no syntax hazard
//! ([`plain_syntax_ok`]). When in doubt, the string is quoted.
//!
//! Every layout (`Layout::SerdeYaml` included) also has a floor: the text
//! must read back as a string under the settings the output is read with
//! (the caller's YAML 1.1 behaviours and merge keys).

use std::fmt::Write as _;

use oneyaml_core::{
    Compat, Diagnostic, Emit, Label, Layout, QuoteStyle, Schema, Settings, Span, code,
};

use crate::error::Error;
use crate::resolve::{ResolveError, Resolved, Resolver, Style as ResolveStyle, TagInput};
use crate::value::{MAX_CONVERTED_DIGITS, Number, TagStr};

/// Longest implicit key, in bytes of its written form (YAML 1.2.2 §7.4.2:
/// 1024 characters; bytes are never fewer than characters).
const MAX_IMPLICIT_KEY: usize = 1024;
/// `serde_yaml`'s (libyaml's) longest simple key.
const SERDE_YAML_MAX_SIMPLE_KEY: usize = 128;

/// How a string is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    /// Unquoted.
    Plain,
    /// `'…'`, with `'` doubled.
    Single,
    /// `"…"`, with escapes.
    Double,
    /// A `|` literal block (multi-line strings only).
    Literal,
}

/// Where a literal block's content lines would go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockIndent {
    /// Column of the content lines.
    pub(crate) content: usize,
    /// The indentation indicator to print when one is needed: the content
    /// indentation minus the parent node's. `None` where readers disagree
    /// on it, so a text needing one is not written as a block: at the
    /// root, the spec puts the parent at -1 (YAML 1.2.2 §8.1.1.1) and
    /// libyaml-based readers at 0.
    pub(crate) indicator: Option<usize>,
}

/// Which output layout the rules follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    /// The default layout.
    Default,
    /// `Layout::SerdeYaml`: `serde_yaml`'s choices, best effort.
    SerdeYaml,
}

/// The scalar presentation rules for one set of output settings.
#[derive(Clone, Debug)]
pub(crate) struct Rules {
    flavor: Flavor,
    double: bool,
    indent: usize,
    indent_sequences: bool,
    /// Core schema, no 1.1 behaviours: how oneyaml reads its output.
    core: Resolver,
    /// JSON schema.
    json: Resolver,
    /// Core with every YAML 1.1 behaviour on.
    yaml11: Resolver,
    /// Core plus the 1.1 integer forms `serde_yaml` parses (`0b…`, signed
    /// hex): what `serde_yaml` quotes as "would read as a number" (with
    /// [`serde_yaml_signed_octal`], which no setting models).
    serde_yaml: Resolver,
    /// The settings the output will be read back with (the round trip):
    /// the caller's own YAML 1.1 behaviours. A string that resolves to
    /// another type under them is never written plain, in any layout.
    /// `None` when they resolve plain scalars exactly as [`Rules::core`]
    /// does (no second Core check per string).
    active: Option<Resolver>,
    /// `honor_version_directive`: a `%YAML 1.1` document is read with every
    /// 1.1 behaviour, so the floor is [`Rules::yaml11`] too.
    honor_yaml11: bool,
    /// `merge_keys`: a plain `<<` key would merge instead of being the
    /// string key `<<`.
    merge_keys: bool,
    /// `Layout::SerdeYaml` read back with
    /// `Settings::resolve_tagged_variant_content`: a newtype variant around
    /// a null, bool, or number is written `!Name value`.
    tagged_scalar_variants: bool,
    /// The reading settings' `Limits::max_scalar_len`: the longest tag the
    /// output may carry (a longer one would not read back).
    max_tag_len: usize,
    /// Text written into an existing document by the edit layer: a string
    /// stays plain when the document's own schema reads it as a string,
    /// not only when every YAML reader would (the serializer's rule).
    schema_floor: bool,
}

impl Rules {
    /// The rules for `emit`. `Err(Internal)` for a layout or quote style
    /// this version does not know (`-core` enums are
    /// `#[non_exhaustive]`).
    ///
    /// `Y1012` for an indentation outside `1..=Emit::MAX_INDENT`: zero
    /// cannot express nesting, and a wider one would multiply the output
    /// (every line repeats it once per level) into an allocation blow-up
    /// (any value is accepted by the type, never a
    /// panic).
    ///
    /// `reading`: the settings the output is read back with; its schema is
    /// Core here (the serializer and `Document` refuse others first).
    ///
    /// With `reading.honor_version_directive(true)` the floor applies
    /// **all** of YAML 1.1 ([`Compat::yaml_1_1`]) and treats `<<` as a merge
    /// key, for every document, as if each carried `%YAML 1.1`: the rules
    /// cannot know which document the output is read as. Our serializer
    /// never writes `%YAML 1.1`, so this only quotes more than needed; it
    /// never writes a string that reads back as another type.
    pub(crate) fn new(emit: &Emit, reading: &Settings) -> Result<Self, Error> {
        let indent = emit.get_indent();
        if indent == 0 || indent > Emit::MAX_INDENT {
            let d = Diagnostic::error(
                code::INCORRECT_SETTINGS,
                format!(
                    "Emit::indent({indent}) cannot produce correct output: the indentation must be 1..={}",
                    Emit::MAX_INDENT
                ),
                Label::new(Span::empty(0), ""),
            )
            .with_help("use an indentation of 1 to Emit::MAX_INDENT spaces (the default is 2)");
            return Err(Error::from_diagnostic(d));
        }
        let flavor = match emit.get_layout() {
            Layout::Default => Flavor::Default,
            Layout::SerdeYaml => Flavor::SerdeYaml,
            // A layout added to `-core` without an emitter here.
            other => {
                debug_assert!(false, "no emitter for {other:?}");
                return Err(Error::internal(&format!("no emitter for {other:?}"), None));
            }
        };
        let double = match emit.get_quote_style() {
            QuoteStyle::Single => false,
            QuoteStyle::Double => true,
            other => {
                debug_assert!(false, "no emitter for {other:?}");
                return Err(Error::internal(&format!("no emitter for {other:?}"), None));
            }
        };
        Ok(Self {
            flavor,
            double,
            indent,
            indent_sequences: emit.get_indent_sequences(),
            core: Resolver::new(&Settings::new()),
            json: Resolver::new(&Settings::new().schema(Schema::Json)),
            yaml11: Resolver::new(&Compat::yaml_1_1()),
            serde_yaml: Resolver::new(&Settings::new().yaml11_int_forms(true)),
            active: Some(Resolver::new(&reading.clone().schema(Schema::Core)))
                .filter(|r| !r.plain_is_core()),
            honor_yaml11: reading.get_honor_version_directive(),
            merge_keys: reading.get_merge_keys() || reading.get_honor_version_directive(),
            // Only when the output reads back that way.
            tagged_scalar_variants: flavor == Flavor::SerdeYaml
                && reading.get_resolve_tagged_variant_content(),
            max_tag_len: reading.get_limits().get_max_scalar_len(),
            schema_floor: false,
        })
    }

    /// These rules for text an edit writes into a document: a string is
    /// quoted only when the document's schema would read it as another
    /// type, or plain text cannot hold it (the serializer keeps its
    /// stricter every-reader rule).
    pub(crate) fn for_edit(mut self) -> Self {
        self.schema_floor = self.flavor == Flavor::Default;
        self
    }

    /// Whether a tag resolving to `len` bytes reads back
    /// under the reading settings (`max_scalar_len` bounds resolved tags).
    pub(crate) fn tag_fits(&self, len: usize) -> bool {
        len <= self.max_tag_len
    }

    /// The `Y2001` error for a tag of `len` bytes that does not fit.
    pub(crate) fn tag_too_long(&self, len: usize) -> Error {
        Error::new(
            code::SCALAR_TOO_LONG,
            format!(
                "a tag of {len} bytes is longer than Limits::max_scalar_len ({}), which the output \
                 is read back with",
                self.max_tag_len
            ),
            None,
        )
        .with_help("raise Limits::max_scalar_len, or write a shorter tag")
    }

    /// Spaces per indentation level.
    pub(crate) fn indent(&self) -> usize {
        self.indent
    }

    /// Whether block sequences are indented under their key.
    pub(crate) fn indent_sequences(&self) -> bool {
        self.indent_sequences
    }

    /// Whether the rules follow `serde_yaml`'s layout.
    pub(crate) fn serde_yaml_layout(&self) -> bool {
        self.flavor == Flavor::SerdeYaml
    }

    /// Whether a newtype variant around a null, bool, or number is written
    /// as a tagged scalar (`!Square 2.0`, as `serde_yaml` writes it): the
    /// `serde_yaml` layout, read back with
    /// `Settings::resolve_tagged_variant_content`.
    pub(crate) fn tagged_scalar_variants(&self) -> bool {
        self.tagged_scalar_variants
    }

    /// The style for `text` written as a node whose literal block would sit
    /// at `block`.
    pub(crate) fn string_style(&self, text: &str, block: BlockIndent) -> Style {
        match self.flavor {
            Flavor::Default => self.default_style(text, block),
            Flavor::SerdeYaml => self.serde_yaml_style(text, block),
        }
    }

    fn quoted(&self) -> Style {
        if self.double {
            Style::Double
        } else {
            Style::Single
        }
    }

    // The default layout.
    fn default_style(&self, text: &str, block: BlockIndent) -> Style {
        if text.contains('\n') {
            return if literal_ok(text, block, false) {
                Style::Literal
            } else {
                Style::Double
            };
        }
        if has_escape(text) {
            return Style::Double;
        }
        let plain = if self.schema_floor {
            self.plain_schema_ok(text)
        } else {
            self.plain_ok(text)
        };
        if plain { Style::Plain } else { self.quoted() }
    }

    /// Plain when the document's schema reads a string and the text is
    /// safe as plain syntax (the edit layer's rule, see [`Rules::for_edit`]).
    fn plain_schema_ok(&self, text: &str) -> bool {
        let edge = |c: Option<char>| c.is_some_and(char::is_whitespace);
        !edge(text.chars().next())
            && !edge(text.chars().next_back())
            && plain_syntax_ok(text, true)
            && self.reads_back_as_string(text)
    }

    /// Every-reader-safe: plain only when every reader reads a string.
    fn plain_ok(&self, text: &str) -> bool {
        // Conservative: leading or trailing Unicode white space (e.g.
        // U+00A0) is text to YAML but invisible, and tools trim it.
        let edge = |c: Option<char>| c.is_some_and(char::is_whitespace);
        !edge(text.chars().next())
            && !edge(text.chars().next_back())
            && plain_syntax_ok(text, true)
            && reads_as_string(&self.core, text)
            && reads_as_string(&self.json, text)
            && reads_as_string(&self.yaml11, text)
            // The floor ([`Rules::reads_back_as_string`]) minus what was
            // just checked: Core, and all of 1.1.
            && self.reading_extras_ok(text)
            && !foreign_yaml11(text)
    }

    /// The floor of every layout. The text reads back as a string
    /// under the settings it is written for (Core plus the caller's 1.1
    /// behaviours; all of them for `honor_version_directive`).
    fn reads_back_as_string(&self, text: &str) -> bool {
        reads_as_string(&self.core, text)
            && self.reading_extras_ok(text)
            && (!self.honor_yaml11 || reads_as_string(&self.yaml11, text))
    }

    /// The part of the floor beyond Core and `honor_version_directive`: the
    /// caller's own 1.1 behaviours (skipped when they add nothing to Core)
    /// and merge keys.
    fn reading_extras_ok(&self, text: &str) -> bool {
        self.active
            .as_ref()
            .is_none_or(|r| reads_as_string(r, text))
            && !(self.merge_keys && text == "<<")
    }

    // Layout::SerdeYaml: `serde_yaml` requests a literal block for strings
    // with `\n`, single quotes for strings it would read as another type,
    // and otherwise lets libyaml pick plain / single / double. Floor: the
    // result must read back as the same string under our Core reader.
    fn serde_yaml_style(&self, text: &str, block: BlockIndent) -> Style {
        let a = Analysis::of(text);
        if text.contains('\n') {
            return if a.block && literal_ok(text, block, true) {
                Style::Literal
            } else {
                Style::Double
            };
        }
        // Injection: a plain `<<` is the merge key and `=` the
        // YAML 1.1 value key to every merge-key reader (PyYAML, Go, Psych,
        // ours with `merge_keys`), so a string `{"<<": {..}}` would merge
        // into its parent. Always quoted, in every layout (serde_yaml
        // writes them plain: a recorded divergence).
        let typed = !reads_as_string(&self.serde_yaml, text)
            || serde_yaml_signed_octal(text)
            || matches!(text, "<<" | "=");
        let quoted = if a.single && !self.double {
            Style::Single
        } else {
            Style::Double
        };
        if typed || !a.plain || !plain_syntax_ok(text, false) || !self.reads_back_as_string(text) {
            quoted
        } else {
            Style::Plain
        }
    }

    /// Writes `text` in `style` (from [`Rules::string_style`] at the same
    /// `block`). Literal blocks end at their last content line, without a
    /// line break: the caller ends the line.
    pub(crate) fn write_string(
        &self,
        out: &mut String,
        text: &str,
        style: Style,
        block: BlockIndent,
    ) {
        match style {
            Style::Plain => out.push_str(text),
            Style::Single => write_single(out, text),
            Style::Double => self.write_double(out, text),
            Style::Literal => self.write_literal(out, text, block),
        }
    }

    /// Writes the one-line form of a string key (after `tag`, if any) to
    /// `out` and returns `true`; or writes nothing and returns `false` when
    /// the key must use the explicit `? ` form (a multi-line string, one
    /// too long for an implicit key, or an unwritable tag). Written in
    /// place, then measured (no per-key buffer).
    pub(crate) fn write_implicit_key(
        &self,
        out: &mut String,
        text: &str,
        tag: Option<TagStr<'_>>,
    ) -> bool {
        // A key never becomes a literal block: `block` is irrelevant.
        let block = BlockIndent {
            content: self.indent,
            indicator: None,
        };
        let style = self.string_style(text, block);
        if style == Style::Literal {
            return false;
        }
        let start = out.len();
        let mut tag_len = 0usize;
        if let Some(t) = tag {
            // (A tag too long to read back: the node form reports it.)
            if !self.tag_fits(t.len()) || write_tag(out, t).is_err() {
                out.truncate(start);
                return false;
            }
            out.push(' ');
            tag_len = out.len().saturating_sub(start);
        }
        self.write_string(out, text, style, block);
        let fits = match self.flavor {
            Flavor::Default => out.len().saturating_sub(start) <= MAX_IMPLICIT_KEY,
            // libyaml: anchor + tag + value length, and never a key with a
            // line break (`\r`, NEL, LS, PS included) in it.
            Flavor::SerdeYaml => {
                text.len().saturating_add(tag_len.saturating_sub(1)) <= SERDE_YAML_MAX_SIMPLE_KEY
                    && !text.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'])
            }
        };
        if !fits {
            out.truncate(start);
        }
        fits
    }

    /// Whether a one-line scalar of `len` bytes (other than a string) fits
    /// an implicit key.
    pub(crate) fn short_key(&self, len: usize) -> bool {
        match self.flavor {
            Flavor::Default => len <= MAX_IMPLICIT_KEY,
            Flavor::SerdeYaml => len <= SERDE_YAML_MAX_SIMPLE_KEY,
        }
    }

    /// Writes a number: integers in decimal or `0x` hex (never
    /// `0o`; see `Number::to_core_string`); floats per
    /// [`Rules::write_float`].
    ///
    /// `Y3015` (extended 2026-09-28) for a wide integer with
    /// no linear Core form — a **negative** octal or hexadecimal, or a
    /// sexagesimal integer of **either sign** — past the 4,096-digit bound
    /// (its stored digits, octal counted as the hexadecimal it is stored
    /// as, or the digits of its decimal form): converting it to decimal
    /// would cost O(d²), and neither `-0x…` nor base 60 is Core.
    pub(crate) fn write_number(&self, out: &mut String, n: &Number) -> Result<(), Error> {
        match n.as_float() {
            // A float no double holds is written with its exact value.
            Some(f) if !n.is_out_of_range() => {
                self.write_float(out, f);
                Ok(())
            }
            _ => n.write_core(out, MAX_CONVERTED_DIGITS).map_err(|()| {
                Error::new(
                    code::UNREPRESENTABLE_VALUE,
                    format!(
                        "a negative octal or hexadecimal integer, or a sexagesimal (base-60) \
                         integer of either sign, wider than {MAX_CONVERTED_DIGITS} digits has \
                         no linear YAML 1.2 Core form"
                    ),
                    None,
                )
                .with_help(
                    "Core reads `-0x…` and base 60 as strings, and converting to decimal is \
                     quadratic past this bound; write the integer in decimal, or as a string",
                )
            }),
        }
    }

    /// Writes a float: `.inf`, `-.inf`, `.nan`, or the shortest
    /// digits that read back exactly, always with a `.` or an exponent.
    ///
    /// Default layout: exponents are written as `1.0e+16` (a `.` and a
    /// signed exponent), the form YAML 1.1 readers also read as a float
    /// (their float pattern needs both), so the value is safe for every
    /// reader; written in place. `Layout::SerdeYaml`: `serde_yaml`'s
    /// (ryu's) layout, e.g. `1e16`, `0.00001`.
    pub(crate) fn write_float(&self, out: &mut String, f: f64) {
        if f.is_nan() {
            out.push_str(".nan");
        } else if f.is_infinite() {
            out.push_str(if f.is_sign_negative() {
                "-.inf"
            } else {
                ".inf"
            });
        } else {
            match self.flavor {
                Flavor::Default => every_reader_float(out, f),
                Flavor::SerdeYaml => out.push_str(&ryu_layout(f)),
            }
        }
    }

    /// [`Rules::write_float`] into a new string (tests).
    #[cfg(test)]
    pub(crate) fn float_text(&self, f: f64) -> String {
        let mut out = String::new();
        self.write_float(&mut out, f);
        out
    }

    fn write_double(&self, out: &mut String, text: &str) {
        out.push('"');
        for c in text.chars() {
            let escape = match self.flavor {
                Flavor::Default => needs_escape(c),
                // libyaml escapes every non-printable character, including
                // everything outside the Basic Multilingual Plane.
                Flavor::SerdeYaml => needs_escape(c) || !libyaml_printable(c),
            };
            if c == '"' || c == '\\' || escape {
                push_escape(out, c);
            } else {
                out.push(c);
            }
        }
        out.push('"');
    }

    fn write_literal(&self, out: &mut String, text: &str, block: BlockIndent) {
        out.push('|');
        let indicator = match self.flavor {
            Flavor::Default => first_content_line_indented(text),
            // libyaml: when the text starts with a space or a line break.
            Flavor::SerdeYaml => text.starts_with([' ', '\n']),
        };
        if let (true, Some(n)) = (indicator, block.indicator) {
            let _ = write!(out, "{n}");
        }
        let content = text.trim_end_matches('\n');
        let trailing = text.len().saturating_sub(content.len());
        // Clip keeps the final break only after content: a text of only
        // breaks needs keep (as libyaml writes it).
        out.push_str(match trailing {
            0 => "-",
            1 if !content.is_empty() => "",
            _ => "+",
        });
        let body = text.strip_suffix('\n').unwrap_or(text);
        for line in body.split('\n') {
            out.push('\n');
            if !line.is_empty() {
                push_spaces(out, block.content);
                out.push_str(line);
            }
        }
    }
}

/// Writes `null`.
pub(crate) fn write_null(out: &mut String) {
    out.push_str("null");
}

/// Writes `true` or `false`.
pub(crate) fn write_bool(out: &mut String, b: bool) {
    out.push_str(if b { "true" } else { "false" });
}

/// The core tags (`tag:yaml.org,2002:…`) oneyaml's resolver interprets; a
/// `Value::Tagged` with one of them would not read back as a tagged value.
const CORE_TAGS: [&str; 7] = ["null", "bool", "int", "float", "str", "seq", "map"];
const YAML_PREFIX: &str = "tag:yaml.org,2002:";

/// Writes the tag property for a resolved tag: `!local` and
/// `!!name` (for `tag:yaml.org,2002:name`) as shorthands, any other tag
/// verbatim as `!<…>`. ASCII characters a tag cannot hold literally are
/// `%`-escaped, which the reader decodes back.
///
/// # Errors
///
/// `Err(())` for a tag no output reads back as the same custom tag: the
/// empty tag, the core tags the resolver interprets, a tag with a
/// non-ASCII character (a reader keeps the escapes of non-ASCII bytes as
/// written, so no spelling reads back as that character, §5.6), a `%` that
/// is not an escape a reader keeps as written (a bare `%`, or the escape of
/// an ASCII character other than `%`, which a reader decodes), and every
/// tag with no spec-valid form (YAML 1.2.2 §6.8.2.1, Example 6.25): `!`
/// alone (the non-specific tag, `!<!>` is invalid) and a global tag that is
/// not a URI with a scheme (`!<$:?>` is invalid).
#[allow(clippy::result_unit_err, reason = "the caller builds the diagnostic")]
///
/// `tag` may be in two pieces (a shared `%TAG` prefix and its
/// suffix); it is written without joining them.
pub(crate) fn write_tag<'t>(out: &mut String, tag: impl Into<TagStr<'t>>) -> Result<(), ()> {
    let mark = out.len();
    let done = write_tag_checked(out, tag.into());
    if done.is_err() {
        // A refusal in the middle of the text writes nothing.
        out.truncate(mark);
    }
    done
}

fn write_tag_checked(out: &mut String, tag: TagStr<'_>) -> Result<(), ()> {
    if tag.len() == 0 || tag.is("!") || !tag.bytes().all(|b| b.is_ascii()) {
        return Err(());
    }
    if let Some(name) = tag.strip_prefix(YAML_PREFIX) {
        if CORE_TAGS.iter().any(|c| name.is(c)) {
            return Err(());
        }
        if name.len() > 0 {
            out.push_str("!!");
            percent_encode(out, name.chars(), tag_char, true)?;
            return Ok(());
        }
    }
    match tag.strip_prefix("!") {
        Some(local) if local.len() > 0 => write_tag_name(out, local, true)?,
        Some(_) => return Err(()),
        None if !has_uri_scheme(tag) => return Err(()),
        None => {
            out.push_str("!<");
            percent_encode(out, tag.chars(), uri_char, true)?;
            out.push('>');
        }
    }
    Ok(())
}

/// Writes the local tag `!name` for a non-empty `name`: what [`write_tag`]
/// writes for `!name`, without building that string (an enum variant's
/// tag).
///
/// # Errors
///
/// `Err(())` for an empty `name` (`!` alone is the non-specific tag, not
/// a local tag; the caller reports it as `Internal`, never a panic), writing
/// nothing.
#[allow(clippy::result_unit_err, reason = "the caller builds the diagnostic")]
pub(crate) fn write_local_tag<'t>(out: &mut String, name: impl Into<TagStr<'t>>) -> Result<(), ()> {
    write_tag_name(out, name.into(), false)
}

/// `!name` for [`write_local_tag`] (`escapes` false: `name` is a variant
/// name, every `%` in it is a character of the name, and a non-ASCII
/// character is written as its UTF-8 escapes, which a reader matches
/// against the variant after decoding them) and for a tag of a document
/// (`escapes` true, see `percent_encode`).
fn write_tag_name(out: &mut String, name: TagStr<'_>, escapes: bool) -> Result<(), ()> {
    if name.len() == 0 {
        return Err(());
    }
    let mark = out.len();
    out.push('!');
    let done = percent_encode(out, name.chars(), tag_char, escapes);
    if done.is_err() {
        out.truncate(mark);
    }
    done
}

/// RFC 3986 `scheme ":"`: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`
/// then `:`. A verbatim global tag must be a URI (§6.8.2.1).
fn has_uri_scheme(tag: TagStr<'_>) -> bool {
    let mut b = tag.bytes();
    if !b.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in b {
        match c {
            b':' => return true,
            c if c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.') => {}
            _ => return false,
        }
    }
    false
}

/// `ns-uri-char` without `%` (production 39): a `%` is always escaped.
fn uri_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-#;/?:@&=+$,_.!~*'()[]".contains(c)
}

/// `ns-tag-char` without `%` (production 40): no `!` or flow indicators.
pub(crate) fn tag_char(c: char) -> bool {
    uri_char(c) && !"!,[]{}".contains(c)
}

/// Writes `text` with the characters `keep` rejects `%`-escaped (UTF-8).
///
/// With `escapes` (a tag read from a document), a `%XX` the reader keeps as
/// written (an escape of a non-ASCII byte, or `%25`) is written as it is, so
/// that the tag reads back as itself. `Err(())`: a `%` that is none of
/// those, which no reader produces and no spelling reads back.
fn percent_encode<I>(
    out: &mut String,
    mut text: I,
    keep: fn(char) -> bool,
    escapes: bool,
) -> Result<(), ()>
where
    I: Iterator<Item = char> + Clone,
{
    let mut buf = [0u8; 4];
    while let Some(c) = text.next() {
        if c == '%' && escapes {
            let mut ahead = text.clone();
            let (h, l) = (ahead.next(), ahead.next());
            let digit = |c: Option<char>| c.and_then(|c| c.to_digit(16));
            let (Some(hi), Some(lo), Some(h), Some(l)) = (digit(h), digit(l), h, l) else {
                return Err(());
            };
            let v = hi.saturating_mul(16).saturating_add(lo);
            if v < 0x80 && v != 0x25 {
                return Err(());
            }
            out.push('%');
            out.push(h);
            out.push(l);
            text = ahead;
        } else if keep(c) {
            out.push(c);
        } else {
            for b in c.encode_utf8(&mut buf).bytes() {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    Ok(())
}

/// Writes `n` spaces, a slice at a time (not a `char` at a time).
pub(crate) fn push_spaces(out: &mut String, n: usize) {
    const SPACES: &str = "                                                                ";
    let mut left = n;
    while left > 0 {
        let take = left.min(SPACES.len());
        out.push_str(SPACES.get(..take).unwrap_or(""));
        left = left.saturating_sub(take);
    }
}

/// `text.chars().any(needs_escape)`, over the UTF-8 bytes: C0
/// controls and DEL are single bytes; C1 controls (U+0080–U+009F) are
/// `C2 80..9F`; U+2028/U+2029 are `E2 80 A8/A9`; U+FEFF is `EF BB BF`;
/// U+FFFE/U+FFFF are `EF BF BE/BF`. Continuation bytes (`80..BF`) never
/// equal the lead bytes matched here, so a match starts at a character.
fn has_escape(text: &str) -> bool {
    let mut s = text.as_bytes();
    loop {
        match s {
            [] => return false,
            [0x00..=0x1f | 0x7f, ..]
            | [0xc2, 0x80..=0x9f, ..]
            | [0xe2, 0x80, 0xa8 | 0xa9, ..]
            | [0xef, 0xbb, 0xbf, ..]
            | [0xef, 0xbf, 0xbe | 0xbf, ..] => return true,
            [_, rest @ ..] => s = rest,
        }
    }
}

/// Whether the plain `text` reads as a string (or, under JSON, as nothing:
/// "no match" is not another type). A resolver failure counts as "not a
/// string", so the text gets quoted.
/// `serde_yaml`'s reader also takes a signed `0o` as an integer
/// (`[-+]0o[0-7]+`: `-0o17` is -15), which is neither YAML 1.1 nor 1.2 and
/// so is in no resolver setting. Quoted like its other
/// numbers.
fn serde_yaml_signed_octal(text: &str) -> bool {
    text.strip_prefix(['-', '+'])
        .and_then(|t| t.strip_prefix("0o"))
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|c| matches!(c, b'0'..=b'7')))
}

fn reads_as_string(r: &Resolver, text: &str) -> bool {
    matches!(
        r.resolve_scalar(TagInput::Absent, ResolveStyle::Plain, text),
        Ok(Resolved::Str) | Err(ResolveError::JsonUnmatched)
    )
}

/// Characters written escaped in double quotes and never plain or in single
/// quotes: C0 controls (tab and line breaks included), DEL, C1 controls
/// (NEL is a line break in YAML 1.1), the Unicode line and paragraph
/// separators (line breaks in YAML 1.1), the byte-order mark, and the
/// non-characters U+FFFE/U+FFFF (outside `c-printable`, production 1).
fn needs_escape(c: char) -> bool {
    matches!(c, '\0'..='\x1f' | '\x7f'..='\u{9f}' | '\u{2028}' | '\u{2029}' | '\u{feff}' | '\u{fffe}' | '\u{ffff}')
}

fn push_escape(out: &mut String, c: char) {
    let short = match c {
        '\0' => "\\0",
        '\x07' => "\\a",
        '\x08' => "\\b",
        '\t' => "\\t",
        '\n' => "\\n",
        '\x0b' => "\\v",
        '\x0c' => "\\f",
        '\r' => "\\r",
        '\x1b' => "\\e",
        '"' => "\\\"",
        '\\' => "\\\\",
        '\u{85}' => "\\N",
        '\u{2028}' => "\\L",
        '\u{2029}' => "\\P",
        _ => "",
    };
    if !short.is_empty() {
        out.push_str(short);
        return;
    }
    let n = u32::from(c);
    let _ = if n <= 0xff {
        write!(out, "\\x{n:02X}")
    } else if n <= 0xffff {
        write!(out, "\\u{n:04X}")
    } else {
        write!(out, "\\U{n:08X}")
    };
}

fn write_single(out: &mut String, text: &str) {
    out.push('\'');
    for c in text.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
}

/// Syntax hazards of a plain scalar in block context (YAML 1.2.2
/// §7.3.3): empty, leading or trailing white space, a leading indicator,
/// `: ` / ` #` / a final `:`, a document marker, or a character that needs
/// escaping. `conservative` (the default layout) also refuses a leading `-`,
/// `?`, or `:` where the spec allows it (before a non-blank, production
/// 126), as `serde_yaml`'s libyaml does not.
///
/// One pass over the bytes. Every hazard is ASCII, and
/// the bytes of a multi-byte character are all `>= 0x80`, so a byte test
/// decides exactly what the character test did (the reference form is
/// kept in the tests, `plain_syntax_reference`).
fn plain_syntax_ok(text: &str, conservative: bool) -> bool {
    let b = text.as_bytes();
    let Some(&first) = b.first() else {
        return false;
    };
    let spec_plain_first =
        matches!(first, b'-' | b'?' | b':') && b.get(1).is_some_and(|c| !matches!(c, b' ' | b'\t'));
    let indicator = matches!(
        first,
        b'-' | b'?'
            | b':'
            | b','
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'#'
            | b'&'
            | b'*'
            | b'!'
            | b'|'
            | b'>'
            | b'\''
            | b'"'
            | b'%'
            | b'@'
            | b'`'
    ) && (conservative || !spec_plain_first);
    if indicator
        || matches!(first, b' ' | b'\t')
        || matches!(b.last(), Some(b' ' | b'\t' | b':'))
        || b.starts_with(b"---")
        || b.starts_with(b"...")
    {
        return false;
    }
    let mut prev = 0u8;
    for &c in b {
        if (prev == b':' && c == b' ') || (prev == b' ' && c == b'#') {
            return false;
        }
        prev = c;
    }
    !has_escape(text)
}

/// YAML 1.1 types our resolver does not model, which 1.1 readers (PyYAML,
/// Go `yaml.v2`, …) would not read as a string (yaml.org/type): the merge
/// key `<<`, the value key `=`, timestamps (`2001-12-14`, with or without
/// a time), the full 1.1 float pattern (`1.2.3`, `.`), and 1.1 integers of
/// only `_` digits (`0x_`, `0b_`).
///
/// Also the non-string readings of the other readers the every-reader rule
/// names: Ruby Psych ([`psych_number`]) and Go `yaml.v2`/`v3` ([`go_number`]).
fn foreign_yaml11(text: &str) -> bool {
    // Text led by an ASCII letter other than `e`/`E`
    // can only be one of Psych's words. Every other form needs a digit,
    // sign, `.`, `<`, or `=` first, and Psych's comma-grouped numbers
    // allow no letter but `e`/`E` (the reference is kept in the tests).
    // MSRV 1.85 has no let-chains.
    if let Some(&c) = text.as_bytes().first() {
        if c.is_ascii_alphabetic() && !matches!(c, b'e' | b'E') {
            return psych_word(text);
        }
    }
    matches!(text, "<<" | "=")
        || timestamp_like(text)
        || yaml11_float(text)
        || underscore_int(text)
        || prefixed_int_any_case(text)
        || signed_twice(text)
        || psych_number(text)
        || go_number(text)
}

/// Two or more signs and then a digit, `.` or `_` (`+-1`, `++2`, `-+1.5`):
/// a reader that strips one sign and parses the rest with a parser that
/// takes a sign again reads a number.
fn signed_twice(text: &str) -> bool {
    let b = text.as_bytes();
    let sign = |c: &u8| matches!(c, b'-' | b'+');
    b.iter().take_while(|c| sign(c)).count() >= 2
        && b.iter()
            .find(|c| !sign(c))
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'_'))
}

/// Ruby Psych's scalar scanner (one of the readers every plain scalar must
/// suit), conservatively:
///
/// * `null`, `yes`/`true`/`on`, and `no`/`false`/`off` in **any** letter
///   case (`tRUE`, `nULL`, `yEs`, `oFF`): Psych matches them with `/i`;
/// * (in [`prefixed_int_any_case`]) binary and hexadecimal with `,`
///   anywhere after the prefix (`0b1,0`, `0x1,F`, `0x,1F`): Psych's legacy
///   integer pattern allows `,` there and deletes `,`/`_` before parsing;
/// * comma-grouped numbers (`1,000`, `1,2`, `+1,000`, `0,7`, `1,000.5`,
///   `1,000e+3`): Psych deletes `,` and `_` before parsing, so any text of
///   only digits, `,`, `_`, `.`, and an exponent, with a `,` and a digit,
///   is quoted;
/// * base 60 led by **any** digit (`09:15`, `00:30`, `0:30`, `1:2.5`):
///   `[-+]?[0-9][0-9_]*(:[0-9]{1,2})+(\.[0-9_]*)?`;
/// * `.inf`, `+.inf`, `-.inf`, `.nan` in any letter case (`.iNf`).
fn psych_number(text: &str) -> bool {
    if psych_word(text) {
        return true;
    }
    let s = text.strip_prefix(['-', '+']).unwrap_or(text);
    if s.eq_ignore_ascii_case(".inf") || s.eq_ignore_ascii_case(".nan") {
        return true;
    }
    let numeric =
        |c: u8| c.is_ascii_digit() || matches!(c, b',' | b'_' | b'.' | b'e' | b'E' | b'+' | b'-');
    if s.contains(',') && s.bytes().any(|c| c.is_ascii_digit()) && s.bytes().all(numeric) {
        return true;
    }
    psych_base60(s)
}

/// Psych's null and boolean words, in any letter case.
fn psych_word(text: &str) -> bool {
    const WORDS: [&str; 7] = ["null", "yes", "true", "on", "no", "false", "off"];
    (2..=5).contains(&text.len()) && WORDS.iter().any(|w| text.eq_ignore_ascii_case(w))
}

/// `[0-9][0-9_]*(:[0-9]{1,2})+(\.[0-9_]*)?` (after the sign).
fn psych_base60(s: &str) -> bool {
    let b = s.as_bytes();
    if !b.first().is_some_and(u8::is_ascii_digit) {
        return false;
    }
    let mut i = b
        .iter()
        .take_while(|c| c.is_ascii_digit() || **c == b'_')
        .count();
    let mut groups = 0usize;
    while b.get(i) == Some(&b':') {
        i = i.saturating_add(1);
        let n = b
            .iter()
            .skip(i)
            .take(2)
            .take_while(|c| c.is_ascii_digit())
            .count();
        if n == 0 {
            return false;
        }
        i = i.saturating_add(n);
        groups = groups.saturating_add(1);
    }
    if groups == 0 {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i = i.saturating_add(1);
        i = i.saturating_add(
            b.iter()
                .skip(i)
                .take_while(|c| c.is_ascii_digit() || **c == b'_')
                .count(),
        );
    }
    i == b.len()
}

/// Go `yaml.v2`/`v3` (one of the readers every plain scalar must suit): a
/// plain scalar starting with a digit, a sign, or `.` has **every** `_`
/// removed, then is parsed
/// with `strconv.ParseInt`/`ParseUint` (base prefix `0x`/`0o`/`0b`, any
/// case, or a leading `0` for octal) or, when it matches the YAML float
/// pattern, `strconv.ParseFloat` (`+_1`, `1e_5`, `0_x1F`, `1_e_5`).
fn go_number(text: &str) -> bool {
    if !text.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | '_')) {
        return false;
    }
    if !text.contains('_') {
        return go_int(text.as_bytes()) || go_float(text.as_bytes());
    }
    // Only number-like text with `_` gets here: the copy is rare.
    let plain: Vec<u8> = text.bytes().filter(|c| *c != b'_').collect();
    go_int(&plain) || go_float(&plain)
}

/// `strconv.ParseInt(s, 0, _)` syntax (any width).
fn go_int(s: &[u8]) -> bool {
    let s = s
        .strip_prefix(b"-")
        .or_else(|| s.strip_prefix(b"+"))
        .unwrap_or(s);
    let digits = |d: &[u8], ok: fn(&u8) -> bool| !d.is_empty() && d.iter().all(ok);
    match s {
        [b'0', b'x' | b'X', rest @ ..] => digits(rest, u8::is_ascii_hexdigit),
        [b'0', b'b' | b'B', rest @ ..] => digits(rest, |c| matches!(c, b'0' | b'1')),
        [b'0', b'o' | b'O', rest @ ..] => digits(rest, |c| matches!(c, b'0'..=b'7')),
        _ => digits(s, u8::is_ascii_digit),
    }
}

/// yaml.v2/v3 `yamlStyleFloat`:
/// `^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$`.
fn go_float(s: &[u8]) -> bool {
    let s = s
        .strip_prefix(b"-")
        .or_else(|| s.strip_prefix(b"+"))
        .unwrap_or(s);
    let run = |s: &[u8], at: usize| s.iter().skip(at).take_while(|c| c.is_ascii_digit()).count();
    let mut i;
    if s.first() == Some(&b'.') {
        let n = run(s, 1);
        if n == 0 {
            return false;
        }
        i = n.saturating_add(1);
    } else {
        i = run(s, 0);
        if i == 0 {
            return false;
        }
        if s.get(i) == Some(&b'.') {
            i = i.saturating_add(1);
            i = i.saturating_add(run(s, i));
        }
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        i = i.saturating_add(1);
        if matches!(s.get(i), Some(b'-' | b'+')) {
            i = i.saturating_add(1);
        }
        let n = run(s, i);
        if n == 0 {
            return false;
        }
        i = i.saturating_add(n);
    }
    i == s.len()
}

/// `[-+]?0[xXoObB][-+]?[0-9a-fA-F_,]+`: radix prefixes in any case, which some
/// readers (serde-saphyr: `0X1F`) take for numbers though no schema does,
/// and with `,` among the digits, which Ruby Psych's legacy integer
/// pattern accepts for binary and hexadecimal (`0b1,0` is 2, `0x,1F` is
/// 31: it deletes `,` and `_` before parsing). A sign after the prefix
/// (`0b-1`, `0X+F`) is quoted too: some readers strip it and parse the rest.
fn prefixed_int_any_case(text: &str) -> bool {
    let s = text.strip_prefix(['-', '+']).unwrap_or(text);
    let mut b = s.bytes();
    if b.next() != Some(b'0') || !matches!(b.next(), Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B'))
    {
        return false;
    }
    let digits = s.get(2..).unwrap_or("");
    let digits = digits.strip_prefix(['-', '+']).unwrap_or(digits);
    !digits.is_empty()
        && digits
            .bytes()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, b'_' | b','))
}

/// `[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}` followed by the end, `T`/`t`, or
/// white space: conservative for yaml.org/type/timestamp.
fn timestamp_like(text: &str) -> bool {
    let b = text.as_bytes();
    let digits = |from: usize, min: usize, max: usize| -> Option<usize> {
        let n = b
            .iter()
            .skip(from)
            .take(max)
            .take_while(|c| c.is_ascii_digit())
            .count();
        (n >= min).then(|| from.saturating_add(n))
    };
    let dash = |at: usize| (b.get(at) == Some(&b'-')).then(|| at.saturating_add(1));
    let end = digits(0, 4, 4)
        .and_then(dash)
        .and_then(|i| digits(i, 1, 2))
        .and_then(dash)
        .and_then(|i| digits(i, 1, 2));
    match end {
        Some(i) => matches!(b.get(i), None | Some(b'T' | b't' | b' ' | b'\t')),
        None => false,
    }
}

/// The YAML 1.1 float pattern (yaml.org/type/float):
/// `[-+]?([0-9][0-9_]*)?\.[0-9.]*([eE][-+][0-9]+)?`.
fn yaml11_float(text: &str) -> bool {
    let s = text.strip_prefix(['-', '+']).unwrap_or(text);
    let b = s.as_bytes();
    let mut i = 0usize;
    if b.first().is_some_and(u8::is_ascii_digit) {
        i = b
            .iter()
            .take_while(|c| c.is_ascii_digit() || **c == b'_')
            .count();
    }
    if b.get(i) != Some(&b'.') {
        return false;
    }
    i = i.saturating_add(1);
    i = i.saturating_add(
        b.iter()
            .skip(i)
            .take_while(|c| c.is_ascii_digit() || **c == b'.')
            .count(),
    );
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i = i.saturating_add(1);
        if !matches!(b.get(i), Some(b'-' | b'+')) {
            return false;
        }
        i = i.saturating_add(1);
        let n = b.iter().skip(i).take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        i = i.saturating_add(n);
    }
    i == b.len()
}

/// `[-+]?0[xb]_+`: 1.1 integer patterns allow digit runs of only `_`.
fn underscore_int(text: &str) -> bool {
    let s = text.strip_prefix(['-', '+']).unwrap_or(text);
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0b"))
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|c| c == b'_'))
}

/// Whether the first non-empty line of `text` starts with a space, so a
/// literal block needs an indentation indicator (auto-detection would
/// misread it, §8.1.1.1).
fn first_content_line_indented(text: &str) -> bool {
    text.split('\n')
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.starts_with(' '))
}

/// Whether `text` can be the content of a literal or folded block scalar:
/// nothing that needs an escape other than a line feed or a tab, and no tab
/// at the start of the first content line (readers take it for indentation).
pub(crate) fn block_text_ok(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c != '\n' && c != '\t' && needs_escape(c))
        && !text
            .split('\n')
            .find(|l| !l.is_empty())
            .is_some_and(|l| l.starts_with('\t'))
}

/// Whether a literal block represents `text` exactly at `block`.
///
/// Default: some line has content (strings of only line breaks are
/// double-quoted), no character needs escaping other than `\n` and tab,
/// and an indentation indicator, if needed, is a single digit.
/// `serde_yaml` layout (`libyaml`): libyaml's own conditions were checked
/// by the caller; only the indicator limit applies here.
fn literal_ok(text: &str, block: BlockIndent, serde_yaml: bool) -> bool {
    // libyaml-based readers reject a tab at the start of the first content
    // line (they take it for indentation), so that text is double-quoted.
    let chars_ok = serde_yaml
        || (text.chars().any(|c| c != '\n')
            && !text
                .chars()
                .any(|c| c != '\n' && c != '\t' && needs_escape(c))
            && !text
                .split('\n')
                .find(|l| !l.is_empty())
                .is_some_and(|l| l.starts_with('\t')));
    let indicator = if serde_yaml {
        text.starts_with([' ', '\n'])
    } else {
        first_content_line_indented(text)
    };
    chars_ok && (!indicator || block.indicator.is_some_and(|n| (1..=9).contains(&n)))
}

/// `IS_PRINTABLE` of the libyaml `serde_yaml_ng` uses (`unsafe-libyaml`):
/// `\n`, `0x20–0x7E`, `U+00A0–U+D7FF`, `U+E000–U+FFFD` except U+FEFF, and
/// everything outside the Basic Multilingual Plane.
fn libyaml_printable(c: char) -> bool {
    matches!(c, '\n' | ' '..='~' | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..)
        && c != '\u{feff}'
}

/// libyaml's `yaml_emitter_analyze_scalar`, for block context with
/// unicode output on: which styles libyaml allows for a string.
#[derive(Clone, Copy, Debug)]
struct Analysis {
    plain: bool,
    single: bool,
    block: bool,
}

impl Analysis {
    fn of(text: &str) -> Self {
        if text.is_empty() {
            return Self {
                plain: false,
                single: true,
                block: false,
            };
        }
        let is_break = |c: char| matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}');
        let is_blankz = |c: Option<char>| c.is_none_or(|c| matches!(c, ' ' | '\t') || is_break(c));
        let mut block_indicators = text.starts_with("---") || text.starts_with("...");
        let (mut special, mut line_breaks) = (false, false);
        let (mut leading_space, mut leading_break) = (false, false);
        let (mut trailing_space, mut trailing_break) = (false, false);
        let (mut break_space, mut space_break) = (false, false);
        let (mut previous_space, mut previous_break) = (false, false);
        let mut preceded_by_ws = true;
        // One pass with one character of look-ahead, no buffer.
        let mut chars = text.chars().peekable();
        let mut first = true;
        while let Some(c) = chars.next() {
            let next = chars.peek().copied();
            let is_last = next.is_none();
            let followed_by_ws = is_blankz(next);
            if first {
                if "#,[]{}&*!|>'\"%@`".contains(c) || (c == '-' && followed_by_ws) {
                    block_indicators = true;
                }
                if (c == '?' || c == ':') && followed_by_ws {
                    block_indicators = true;
                }
            } else if (c == ':' && followed_by_ws) || (c == '#' && preceded_by_ws) {
                block_indicators = true;
            }
            if !libyaml_printable(c) {
                special = true;
            }
            if is_break(c) {
                line_breaks = true;
            }
            if c == ' ' {
                leading_space |= first;
                trailing_space |= is_last;
                break_space |= previous_break;
                previous_space = true;
                previous_break = false;
            } else if is_break(c) {
                leading_break |= first;
                trailing_break |= is_last;
                space_break |= previous_space;
                previous_break = true;
                previous_space = false;
            } else {
                previous_space = false;
                previous_break = false;
            }
            preceded_by_ws = is_blankz(Some(c));
            first = false;
        }
        let mut a = Self {
            plain: true,
            single: true,
            block: true,
        };
        if leading_space || leading_break || trailing_space || trailing_break {
            a.plain = false;
        }
        if trailing_space {
            a.block = false;
        }
        if break_space || space_break || special {
            a.plain = false;
            a.single = false;
        }
        if space_break || special {
            a.block = false;
        }
        if line_breaks || block_indicators {
            a.plain = false;
        }
        // Deviation (listed in the compat divergences): libyaml would write
        // U+2028/U+2029 as line breaks inside single quotes, which YAML 1.2
        // reads as other text. Double quotes escape them.
        if text.contains(['\u{2028}', '\u{2029}']) {
            a.single = false;
        }
        a
    }
}

/// `{:e}` split into (digits without `.`, decimal exponent of the first).
fn sci_parts(f: f64) -> (String, i32) {
    let s = format!("{:e}", f.abs());
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    (digits, exp.parse().unwrap_or(0))
}

/// The default float text: Rust's shortest round-trip form, with an
/// exponent always written as `d.ddde±X` (see [`Rules::write_float`]),
/// written and fixed up in place in `out`.
fn every_reader_float(out: &mut String, f: f64) {
    let start = out.len();
    let _ = write!(out, "{f:?}");
    let Some(e) = out
        .get(start..)
        .and_then(|s| s.find('e'))
        .map(|i| start.saturating_add(i))
    else {
        return;
    };
    let after = e.saturating_add(1);
    if !out.get(after..).is_some_and(|x| x.starts_with('-')) {
        out.insert(after, '+');
    }
    if !out.get(start..e).is_some_and(|m| m.contains('.')) {
        out.insert_str(e, ".0");
    }
}

/// ryu's `format64` layout from the shortest digits (what `serde_yaml`
/// prints for an `f64`).
fn ryu_layout(f: f64) -> String {
    let mut out = String::new();
    if f.is_sign_negative() {
        out.push('-');
    }
    if f == 0.0 {
        out.push_str("0.0");
        return out;
    }
    let (digits, e) = sci_parts(f);
    let length = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    // `kk`: position of the decimal point; `k`: exponent of the last digit.
    let kk = i64::from(e).saturating_add(1);
    let k = kk.saturating_sub(length);
    if (0..=16).contains(&kk) && k >= 0 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', usize::try_from(k).unwrap_or(0)));
        out.push_str(".0");
    } else if 0 < kk && kk <= 16 {
        let at = usize::try_from(kk).unwrap_or(0);
        let (a, b) = digits.split_at(at.min(digits.len()));
        out.push_str(a);
        out.push('.');
        out.push_str(b);
    } else if -5 < kk && kk <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n(
            '0',
            usize::try_from(kk.saturating_neg()).unwrap_or(0),
        ));
        out.push_str(&digits);
    } else {
        let mut chars = digits.chars();
        if let Some(d) = chars.next() {
            out.push(d);
        }
        let rest: String = chars.collect();
        if !rest.is_empty() {
            out.push('.');
            out.push_str(&rest);
        }
        let _ = write!(out, "e{}", kk.saturating_sub(1));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn rules(emit: Emit) -> Rules {
        Rules::new(&emit, &Settings::new()).unwrap()
    }

    const NODE: BlockIndent = BlockIndent {
        content: 2,
        indicator: Some(2),
    };

    fn written(r: &Rules, s: &str) -> String {
        let mut out = String::new();
        let style = r.string_style(s, NODE);
        r.write_string(&mut out, s, style, NODE);
        out
    }

    #[test]
    fn every_reader_safe_quoting() {
        let r = rules(Emit::new());
        for s in [
            "",
            " a",
            "a ",
            "#a",
            "a #b",
            "a: b",
            "a:",
            "-",
            "-a",
            "?a",
            ":a",
            "no",
            "yes",
            "on",
            "off",
            "y",
            "n",
            "Y",
            "~",
            "0777",
            "1_000",
            "12:30",
            ".inf",
            "null",
            "123",
            "0x1F",
            "0b1",
            "1e3",
            "1.2.3",
            ".",
            "2001-12-14",
            "2001-1-2t",
            "<<",
            "=",
            "---",
            "...",
            "true",
            "NULL",
            "+1",
            "0o7",
            "0x_",
            "-0b_",
            "@a",
            "`a",
            "%a",
            "!a",
            "&a",
            "*a",
            "|a",
            ">a",
            "[a",
            "]a",
            "{a",
            "}a",
            ",a",
            "'a",
            "\"a",
        ] {
            let w = written(&r, s);
            assert!(w.starts_with('\''), "{s:?} => {w:?}");
        }
        for s in ["\u{a0}", "a\u{2003}", "\u{3000}a"] {
            assert_eq!(written(&r, s), format!("'{s}'"), "{s:?}");
        }
        for s in [
            "a", "a b", "a,b", "a[b", "é", "a'b", "a\"b", "2001-12", "x.y", "1.2.x", "a\u{a0}b",
        ] {
            assert_eq!(written(&r, s), s, "{s:?}");
        }
        assert_eq!(written(&r, "a'b "), "'a''b '");
        assert_eq!(written(&r, "a\tb"), "\"a\\tb\"");
        assert_eq!(written(&r, "a\rb"), "\"a\\rb\"");
        assert_eq!(
            written(&r, "\u{1}\u{7f}\u{85}\u{feff}\u{2028}\u{ffff}\u{a0}"),
            "\"\\x01\\x7F\\N\\uFEFF\\L\\uFFFF\u{a0}\""
        );
        assert_eq!(written(&r, "\\ \""), "\\ \"");
        let d = rules(Emit::new().quote_style(QuoteStyle::Double));
        assert_eq!(written(&d, "yes"), "\"yes\"");
        assert_eq!(written(&d, "a"), "a");
    }

    #[test]
    fn literal_blocks_and_chomping() {
        let r = rules(Emit::new());
        assert_eq!(written(&r, "a\nb"), "|-\n  a\n  b");
        assert_eq!(written(&r, "a\nb\n"), "|\n  a\n  b");
        assert_eq!(written(&r, "a\nb\n\n"), "|+\n  a\n  b\n");
        assert_eq!(written(&r, "\na"), "|-\n\n  a");
        assert_eq!(written(&r, " a\nb"), "|2-\n   a\n  b");
        assert_eq!(written(&r, "\n a"), "|2-\n\n   a");
        assert_eq!(written(&r, "a\n\tb"), "|-\n  a\n  \tb");
        assert_eq!(written(&r, "\ta\nb"), "\"\\ta\\nb\"");
        assert_eq!(written(&r, "\n\ta"), "\"\\n\\ta\"");
        // Only line breaks, or characters a block cannot hold: double.
        assert_eq!(written(&r, "\n"), "\"\\n\"");
        assert_eq!(written(&r, "\n\n"), "\"\\n\\n\"");
        assert_eq!(written(&r, "a\r\nb"), "\"a\\r\\nb\"");
        // An indicator must be one digit.
        let wide = BlockIndent {
            content: 10,
            indicator: Some(10),
        };
        assert_eq!(r.string_style(" a\nb", wide), Style::Double);
        assert_eq!(r.string_style("a\nb", wide), Style::Literal);
        // No indicator possible (the root): double quotes.
        let root = BlockIndent {
            content: 2,
            indicator: None,
        };
        assert_eq!(r.string_style(" a\nb", root), Style::Double);
        assert_eq!(r.string_style("a\nb", root), Style::Literal);
    }

    #[test]
    fn floats() {
        let r = rules(Emit::new());
        let s = rules(Emit::new().layout(Layout::SerdeYaml));
        for (f, ours, theirs) in [
            (1.0, "1.0", "1.0"),
            (0.1, "0.1", "0.1"),
            (1e300, "1.0e+300", "1e300"),
            (1e16, "1.0e+16", "1e16"),
            (1e15, "1000000000000000.0", "1000000000000000.0"),
            (1e-5, "1.0e-5", "0.00001"),
            (1e-7, "1.0e-7", "1e-7"),
            (1.5e-7, "1.5e-7", "1.5e-7"),
            (-0.0, "-0.0", "-0.0"),
            (123_456_789.125, "123456789.125", "123456789.125"),
            (1.25e21, "1.25e+21", "1.25e21"),
            (-2.5e-300, "-2.5e-300", "-2.5e-300"),
            (f64::INFINITY, ".inf", ".inf"),
            (f64::NEG_INFINITY, "-.inf", "-.inf"),
            (f64::NAN, ".nan", ".nan"),
        ] {
            assert_eq!(r.float_text(f), ours, "{f:?}");
            assert_eq!(s.float_text(f), theirs, "{f:?}");
            if f.is_finite() {
                assert_eq!(ours.parse::<f64>().unwrap().to_bits(), f.to_bits());
            }
        }
    }

    #[test]
    fn tags() {
        let t = |tag: &str| {
            let mut out = String::new();
            write_tag(&mut out, tag).map(|()| out)
        };
        assert_eq!(t("!color").unwrap(), "!color");
        assert_eq!(t("!a!b c").unwrap(), "!a%21b%20c");
        // A reader keeps the escapes of non-ASCII bytes as written, so a tag
        // with such a character has no written form.
        assert!(t("!é").is_err());
        // §6.8.2.1 Example 6.25: `!<!>` and `!<$:?>` are invalid, so
        // those tags have no written form.
        assert!(t("!").is_err());
        assert!(t("$:?").is_err());
        for bad in ["x", "a b", ":x", "1a:b", "a_b:c", "é:x"] {
            assert!(t(bad).is_err(), "{bad}");
        }
        assert_eq!(t("urn:x").unwrap(), "!<urn:x>");
        assert_eq!(t("a+b.c-d:x y").unwrap(), "!<a+b.c-d:x%20y>");
        assert_eq!(t("!!").unwrap(), "!%21");
        assert_eq!(t("tag:yaml.org,2002:binary").unwrap(), "!!binary");
        assert_eq!(
            t("tag:example.com,2000:a/b").unwrap(),
            "!<tag:example.com,2000:a/b>"
        );
        assert_eq!(t("tag:yaml.org,2002:").unwrap(), "!<tag:yaml.org,2002:>");
        assert!(t("").is_err());
        assert!(t("tag:yaml.org,2002:str").is_err());
    }

    /// The every-reader rule's other readers: Psych and Go.
    #[test]
    fn psych_and_go_readings_are_quoted() {
        let r = rules(Emit::new());
        for s in [
            // Psych: comma-grouped integers and floats.
            "1,000", "1,2", "+1,000", "-1,000", "0,7", "1,000.5", "1,000e+3", ",1", "1,",
            // Psych: base 60 led by any digit, one or two groups.
            "09:15", "00:30", "0:30", "1:2", "-0:30", "0:30.5", "1_0:30",
            // Psych: `.inf`/`.nan` in any case.
            ".iNf", "+.INf", "-.InF", ".nAn", ".NAn",
            // Psych: null and booleans in any case.
            "tRUE", "nULL", "yEs", "oFF", "FaLsE", "nO", "oN", "NuLl", "TrUe",
            // Psych: `,` in binary and hexadecimal digits.
            "0b1,0", "0x1,F", "0x,1F", "-0b,1", "+0x1,f",
            // Go: every `_` stripped before ParseInt/ParseFloat.
            "+_1", "1e_5", "0_x1F", "1_e_5", "-_1", "1__0", "0_b1", "0_o7", ".5_", "1._5",
        ] {
            assert!(foreign_yaml11(s), "{s} is foreign");
            let w = written(&r, s);
            assert_eq!(w, format!("'{s}'"), "{s:?}");
        }
        // Non-numbers stay plain.
        for s in ["a,b", "x_1", "1:a", "infinity_x", "1.2.x"] {
            assert_eq!(written(&r, s), s, "{s:?}");
        }
        // The `serde_yaml` layout keeps serde_yaml's choices.
        let c = rules(Emit::new().layout(Layout::SerdeYaml));
        assert_eq!(written(&c, "1,000"), "1,000");
    }

    #[test]
    fn implicit_keys() {
        // Written after existing text, which a refused key leaves intact.
        let key = |r: &Rules, text: &str, tag: Option<&str>| {
            let mut out = String::from("- ");
            let ok = r.write_implicit_key(&mut out, text, tag.map(TagStr::from));
            assert_eq!(ok, out.len() > 2, "{text:?}");
            ok.then(|| out.split_off(2))
        };
        let r = rules(Emit::new());
        assert_eq!(key(&r, "a", None).unwrap(), "a");
        assert_eq!(key(&r, "yes", Some("!t")).unwrap(), "!t 'yes'");
        assert_eq!(key(&r, "a\nb", None), None);
        assert_eq!(key(&r, "a", Some("!")), None);
        assert!(key(&r, &"x".repeat(1024), None).is_some());
        assert_eq!(key(&r, &"x".repeat(1025), None), None);
        assert_eq!(key(&r, &"x".repeat(1020), Some("!tag")), None);
        let s = rules(Emit::new().layout(Layout::SerdeYaml));
        assert!(key(&s, &"x".repeat(128), None).is_some());
        assert_eq!(key(&s, &"x".repeat(129), None), None);
        assert!(r.short_key(1024) && !r.short_key(1025));
        assert!(s.short_key(128) && !s.short_key(129));
    }

    /// Skipping the reading resolver when it adds nothing to
    /// Core, and not re-checking Core, decide exactly what the full
    /// formula (Core, JSON, 1.1, then Core, active, 1.1 under
    /// `honor_version_directive`, merge `<<`) decides.
    #[test]
    fn reading_floor_shortcut_is_exact() {
        let old_floor = |reading: &Settings, text: &str| {
            let core = Resolver::new(&Settings::new());
            let active = Resolver::new(&reading.clone().schema(Schema::Core));
            let yaml11 = Resolver::new(&Compat::yaml_1_1());
            let honor = reading.get_honor_version_directive();
            let merge = reading.get_merge_keys() || honor;
            reads_as_string(&core, text)
                && reads_as_string(&active, text)
                && (!honor || reads_as_string(&yaml11, text))
                && !(merge && text == "<<")
        };
        let readings = [
            Settings::new(),
            Settings::new().merge_keys(true),
            Settings::new().honor_version_directive(true),
            Settings::new().yaml11_booleans(true),
            Settings::new().legacy_octal(true),
            Settings::new().sexagesimal(true),
            Settings::new().digit_separators(true),
            Settings::new().yaml11_int_forms(true),
            Compat::serde_yaml(),
            Compat::yaml_1_1(),
            Settings::new().schema(Schema::Json),
        ];
        let texts = [
            "a", "yes", "No", "on", "y", "0777", "08", "1_000", "12:30", "0b101", "-0x1F", "+0x1",
            "-0o17", "<<", "=", "null", "~", "1.5", ".inf", "true", "x_1", "a b",
        ];
        for reading in &readings {
            let plain_core = Resolver::new(&reading.clone().schema(Schema::Core)).plain_is_core();
            let expect_core = !(reading.get_yaml11_booleans()
                || reading.get_legacy_octal()
                || reading.get_sexagesimal()
                || reading.get_digit_separators()
                || reading.get_yaml11_int_forms());
            assert_eq!(plain_core, expect_core, "{reading:?}");
            for layout in [Layout::Default, Layout::SerdeYaml] {
                let r = Rules::new(&Emit::new().layout(layout), reading).unwrap();
                assert_eq!(r.active.is_none(), plain_core);
                for t in texts {
                    assert_eq!(r.reads_back_as_string(t), old_floor(reading, t), "{t:?}");
                    let old_plain = !t.starts_with(char::is_whitespace)
                        && !t.ends_with(char::is_whitespace)
                        && plain_syntax_ok(t, true)
                        && reads_as_string(&r.core, t)
                        && reads_as_string(&r.json, t)
                        && reads_as_string(&r.yaml11, t)
                        && old_floor(reading, t)
                        && !foreign_yaml11(t);
                    assert_eq!(r.plain_ok(t), old_plain, "{t:?} {reading:?}");
                }
            }
        }
        // Failsafe and JSON reading schemas are read as Core here (the
        // caller refuses them first); an unknown schema never arises.
        assert!(Resolver::new(&Settings::new()).plain_is_core());
        assert!(!Resolver::new(&Settings::new().schema(Schema::Json)).plain_is_core());
        assert!(!Resolver::new(&Settings::new().schema(Schema::Failsafe)).plain_is_core());
    }

    /// The character-based forms the byte-based fast paths replaced
    /// they must decide exactly the same.
    fn plain_syntax_reference(text: &str, conservative: bool) -> bool {
        let mut chars = text.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        let spec_plain_first =
            "-?:".contains(first) && chars.next().is_some_and(|c| !matches!(c, ' ' | '\t'));
        let indicator =
            "-?:,[]{}#&*!|>'\"%@`".contains(first) && (conservative || !spec_plain_first);
        !(text.starts_with([' ', '\t'])
            || text.ends_with([' ', '\t'])
            || indicator
            || text.contains(": ")
            || text.contains(" #")
            || text.ends_with(':')
            || text.starts_with("---")
            || text.starts_with("...")
            || text.chars().any(needs_escape))
    }

    fn foreign_reference(text: &str) -> bool {
        const WORDS: [&str; 7] = ["null", "yes", "true", "on", "no", "false", "off"];
        matches!(text, "<<" | "=")
            || timestamp_like(text)
            || yaml11_float(text)
            || underscore_int(text)
            || prefixed_int_any_case(text)
            || signed_twice(text)
            || WORDS.iter().any(|w| text.eq_ignore_ascii_case(w))
            || psych_number(text)
            || go_number(text)
    }

    /// Every string of up to three characters over an alphabet of the
    /// characters the rules look at, plus longer samples.
    fn samples() -> Vec<String> {
        let alphabet = [
            "a",
            "e",
            "E",
            "n",
            "N",
            "o",
            "O",
            "f",
            "t",
            "T",
            "y",
            "x",
            "b",
            "0",
            "1",
            "9",
            ".",
            ",",
            "_",
            ":",
            "-",
            "+",
            "?",
            " ",
            "#",
            "<",
            "=",
            "\t",
            "'",
            "é",
            "\u{85}",
            "\u{a0}",
            "\u{2028}",
            "\u{feff}",
            "\u{ffff}",
            "\u{fffd}",
            "\u{1f600}",
            "\u{7f}",
        ];
        let mut out = vec![String::new()];
        for a in alphabet {
            out.push(a.to_owned());
            for b in alphabet {
                out.push(format!("{a}{b}"));
                for c in alphabet {
                    out.push(format!("{a}{b}{c}"));
                }
            }
        }
        for s in [
            "null",
            "NuLl",
            "false",
            "FALSE",
            "off",
            "On",
            "yes",
            "true",
            "e,1",
            "E1,0",
            "e",
            "e_1",
            "eE,+",
            "nulls",
            "no-",
            "1,000",
            "2001-12-14",
            "0x1F",
            "1:30",
            ".inf",
            "a: b",
            "a #b",
            "---a",
            "...a",
            "a:",
            "-a",
            "?a",
            ":a",
            "- a",
            "é: x",
        ] {
            out.push(s.to_owned());
        }
        out
    }

    #[test]
    fn byte_fast_paths_decide_as_before() {
        for s in samples() {
            for conservative in [true, false] {
                assert_eq!(
                    plain_syntax_ok(&s, conservative),
                    plain_syntax_reference(&s, conservative),
                    "{s:?} {conservative}"
                );
            }
            assert_eq!(foreign_yaml11(&s), foreign_reference(&s), "{s:?}");
            assert_eq!(has_escape(&s), s.chars().any(needs_escape), "{s:?}");
        }
        // `has_escape` for every character, alone and between others.
        let mut buf = [0u8; 4];
        for c in (0..=0x10_ffffu32).filter_map(char::from_u32) {
            let one = c.encode_utf8(&mut buf);
            assert_eq!(has_escape(one), needs_escape(c), "{c:?}");
            let mid = format!("a{one}é");
            assert_eq!(has_escape(&mid), needs_escape(c), "{c:?}");
        }
        let mut out = String::new();
        for n in [0, 1, 63, 64, 65, 200] {
            out.clear();
            push_spaces(&mut out, n);
            assert_eq!(out, " ".repeat(n));
        }
        assert!(write_local_tag(&mut out, "").is_err());
    }

    /// A seeded random property over longer strings (up to 12
    /// characters) made of every letter of the Psych/YAML 1.1 words in
    /// both cases and the characters the number patterns look at.
    #[test]
    fn byte_fast_paths_decide_as_before_random() {
        // `i`, `x`, `b` (`inf`, `0x`, `0b`) and
        // non-ASCII (a multi-byte character next to a byte-level check;
        // NEL, NBSP, BOM and LS are YAML-significant or near it).
        let alphabet: Vec<char> =
            "nulyestrofaNULYESTROFAixbIXB0123456789.,_:+-eE~<= #éüß\u{85}\u{a0}\u{feff}\u{2028}☃"
                .chars()
                .collect();
        // xorshift64*, fixed seed: the same strings on every run.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        let words = [
            "null", "yes", "true", "on", "no", "false", "off", "inf", "nan",
        ];
        let mut text = String::new();
        let mut hits = 0usize;
        // 400k rounds: the wider alphabet makes the
        // foreign forms rarer, and both outcomes must stay well covered.
        for round in 0..400_000u32 {
            text.clear();
            // One round in four starts with a word in random letter case,
            // so the words (rare by chance) are well covered.
            if round % 4 == 0 {
                let w = words[(next() % words.len() as u64) as usize];
                for c in w.chars() {
                    let upper = next() % 2 == 0;
                    text.push(if upper { c.to_ascii_uppercase() } else { c });
                }
            }
            let len = (next() % 13) as usize;
            for _ in 0..len {
                text.push(alphabet[(next() % alphabet.len() as u64) as usize]);
            }
            for conservative in [true, false] {
                assert_eq!(
                    plain_syntax_ok(&text, conservative),
                    plain_syntax_reference(&text, conservative),
                    "{text:?} {conservative}"
                );
            }
            let foreign = foreign_yaml11(&text);
            assert_eq!(foreign, foreign_reference(&text), "{text:?}");
            hits += usize::from(foreign);
        }
        // Both outcomes are exercised.
        assert!(hits > 10_000, "{hits}");
    }

    #[test]
    fn foreign_yaml11_forms() {
        for s in [
            "0X1F",
            "-0B1",
            "+0O7_7",
            "<<",
            "=",
            "2001-12-14",
            "2001-12-14T21:59:43.10-05:00",
            "2001-1-1 1:2:3",
            "1.2.3",
            ".",
            "-.5.",
            "1.e+3",
            "0x_",
            "+0b__",
            // Go reads `1.e3` (yamlStyleFloat) as 1000.
            "1.e3",
            // Two signs, a sign after a radix prefix, and `_` removed.
            "+-1",
            "--1",
            "-+1.5",
            "++2",
            "0X-F",
            "0B+1",
            "0b-1",
            "0o+7",
            "0b-1_0",
            "_1",
            "+_1",
            "1e3_",
            "0_x123",
            "1_0.5",
        ] {
            assert!(foreign_yaml11(s), "{s}");
        }
        for s in [
            "2001-12",
            "20011-12-14",
            "2001-12-14x",
            "a.b",
            "0x",
            "0XZ",
            "0y1",
            "a,b",
            "1,a",
            ",",
            "1:a",
            "1:",
            "2001:db8::1",
            "_",
            "a_1",
            "1_a",
            ".in",
            "inf",
            "--",
            "+-",
            "--a",
            "0b-",
            "_",
            "__",
            "_a",
        ] {
            assert!(!foreign_yaml11(s), "{s}");
        }
    }
}
