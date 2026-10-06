//! Output settings.

/// Which quote character the emitter prefers for strings that need quoting.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum QuoteStyle {
    /// Single quotes; double quotes only when escapes are required (the
    /// default).
    #[default]
    Single,
    /// Always double quotes.
    Double,
}

/// Overall output layout.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Layout {
    /// oneyaml's own layout: the defaults.
    #[default]
    Default,
    /// Reproduce `serde_yaml`'s output layout, best effort (checked against
    /// `serde_yaml_ng`). Set by [`Compat::serde_yaml()`](crate::Compat::serde_yaml).
    ///
    /// **Known divergences** (each a deliberate choice: correctness first; the `oracles` crate's `serde_yaml_compat` test
    /// keeps this list exact):
    ///
    /// 1. *Nested enums*: `serde_yaml` refuses a newtype variant around
    ///    another variant ("serializing nested enums in YAML is not
    ///    supported yet"); oneyaml writes the outer one as
    ///    `Name: !Inner ...`, which reads back.
    /// 2. *Enum around a scalar*: `serde_yaml` writes a newtype variant
    ///    around a number, bool, or null as `!Name 1`; a custom-tagged
    ///    scalar always reads back as a string, so oneyaml writes
    ///    `Name: 1`, which both libraries read back as the variant.
    /// 3. *Root indentation indicator*: a root string whose first line
    ///    starts with a space or a line break needs a block indentation
    ///    indicator; libyaml counts it from 0 (`|2`) where YAML 1.2 counts
    ///    from -1 (§8.1.1.1), so readers disagree and oneyaml
    ///    double-quotes it instead.
    /// 4. *U+2028/U+2029*: libyaml writes them as line breaks inside single
    ///    quotes, which YAML 1.2 readers read as other text; oneyaml
    ///    double-quotes them as `\L`/`\P`.
    /// 5. *`char`*: `serde_yaml` always single-quotes a `char`; oneyaml's
    ///    data model keeps a char as a string, quoted only when needed.
    /// 6. *`f32` layout*: `serde_yaml` lays out an `f32` with ryu's f32
    ///    thresholds (`1e14`); oneyaml keeps floats as `f64` and uses the
    ///    f64 thresholds (`100000000000000.0`): same digits, same value.
    SerdeYaml,
}

/// Output settings for the emitter and serializer.
///
/// Consumed by the serializer (`to_string_with`, `to_writer_with`, and
/// `Serializer::with_settings` in `oneyaml`). For an edited `Document`
/// they apply only to new nodes where style inference has no context.
/// Any `indent` value is accepted here; the serializer refuses
/// `indent(0)` (it cannot express nesting) and anything above
/// [`Emit::MAX_INDENT`] with `Y1012` (never a panic or an
/// allocation blow-up for any value).
///
/// Defaults: 2-space indent, single quotes, never wrap, sequences indented
/// under their key, oneyaml's own layout.
///
/// # Examples
///
/// ```
/// use oneyaml_core::{Emit, Layout, QuoteStyle};
///
/// let e = Emit::new()
///     .indent(4)
///     .quote_style(QuoteStyle::Double)
///     .line_width(None)
///     .indent_sequences(false)
///     .layout(Layout::Default);
/// assert_eq!(e.get_indent(), 4);
/// assert_eq!(e.get_quote_style(), QuoteStyle::Double);
/// assert_eq!(e.get_line_width(), None); // never wrap (wrapping is not implemented: `Some` is Y1017)
/// assert!(!e.get_indent_sequences());
/// assert_eq!(e.get_layout(), Layout::Default);
/// ```
// Value type, private fields, chainable `const fn` setters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Emit {
    indent: usize,
    quote_style: QuoteStyle,
    line_width: Option<usize>,
    indent_sequences: bool,
    layout: Layout,
}

impl Emit {
    /// The widest indentation the serializer accepts (`Y1012` above it).
    ///
    /// Block output repeats the indentation once per nesting level on every
    /// line, so the width multiplies output size; the bound keeps any
    /// setting from turning into an allocation blow-up.
    pub const MAX_INDENT: usize = 64;

    /// The defaults. Same as [`Emit::default()`], usable in `const`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            indent: 2,
            quote_style: QuoteStyle::Single,
            line_width: None,
            indent_sequences: true,
            layout: Layout::Default,
        }
    }

    /// Spaces per indentation level (default 2). The serializer accepts
    /// `1..=`[`Emit::MAX_INDENT`]; other values are `Y1012` there.
    #[must_use]
    pub const fn indent(mut self, spaces: usize) -> Self {
        self.indent = spaces;
        self
    }

    /// Preferred quote style (default [`QuoteStyle::Single`]).
    #[must_use]
    pub const fn quote_style(mut self, style: QuoteStyle) -> Self {
        self.quote_style = style;
        self
    }

    /// Wrap long plain and quoted scalars at `width` columns; `None` (the
    /// default) never wraps.
    ///
    /// Wrapping is not implemented yet: the serializer refuses `Some(_)`
    /// with `Y1017` rather than ignoring it silently.
    #[must_use]
    pub const fn line_width(mut self, width: Option<usize>) -> Self {
        self.line_width = width;
        self
    }

    /// Indent a block sequence under its mapping key (`key:\n  - a`, the
    /// default) or not (`key:\n- a`).
    #[must_use]
    pub const fn indent_sequences(mut self, on: bool) -> Self {
        self.indent_sequences = on;
        self
    }

    /// Output layout (default [`Layout::Default`]).
    #[must_use]
    pub const fn layout(mut self, layout: Layout) -> Self {
        self.layout = layout;
        self
    }

    /// The indentation width.
    #[must_use]
    pub const fn get_indent(&self) -> usize {
        self.indent
    }

    /// The preferred quote style.
    #[must_use]
    pub const fn get_quote_style(&self) -> QuoteStyle {
        self.quote_style
    }

    /// The wrap width; `None` never wraps.
    #[must_use]
    pub const fn get_line_width(&self) -> Option<usize> {
        self.line_width
    }

    /// Whether block sequences are indented under their key.
    #[must_use]
    pub const fn get_indent_sequences(&self) -> bool {
        self.indent_sequences
    }

    /// The output layout.
    #[must_use]
    pub const fn get_layout(&self) -> Layout {
        self.layout
    }
}

impl Default for Emit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_setters() {
        let d = Emit::default();
        assert_eq!(d, Emit::new());
        assert_eq!(
            (
                d.get_indent(),
                d.get_quote_style(),
                d.get_line_width(),
                d.get_indent_sequences(),
                d.get_layout()
            ),
            (2, QuoteStyle::Single, None, true, Layout::Default)
        );
        const E: Emit = Emit::new()
            .indent(4)
            .quote_style(QuoteStyle::Double)
            .line_width(Some(80))
            .indent_sequences(false)
            .layout(Layout::SerdeYaml);
        assert_eq!(
            (
                E.get_indent(),
                E.get_quote_style(),
                E.get_line_width(),
                E.get_indent_sequences(),
                E.get_layout()
            ),
            (4, QuoteStyle::Double, Some(80), false, Layout::SerdeYaml)
        );
        assert_eq!(QuoteStyle::default(), QuoteStyle::Single);
        assert_eq!(Layout::default(), Layout::Default);
    }
}
