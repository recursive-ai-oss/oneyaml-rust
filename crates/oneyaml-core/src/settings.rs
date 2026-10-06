//! The single settings value type and its presets.

use crate::{Emit, Layout, Limits};

/// The schema that resolves plain scalars to types.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Schema {
    /// The YAML 1.2 Core schema (the default).
    #[default]
    Core,
    /// The YAML 1.2 JSON schema: strict JSON types.
    Json,
    /// The YAML 1.2 Failsafe schema: every scalar is a string.
    Failsafe,
}

/// What to do with a duplicate mapping key.
///
/// `Document` and the serde deserializer read it alike.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DuplicateKeys {
    /// An error, as YAML 1.2 requires (the default).
    #[default]
    Error,
    /// The last value wins, and a warning with a "first defined here" label
    /// is **always** recorded. Part of [`Compat::serde_yaml()`].
    LastWins,
}

/// All input and output settings.
///
/// A value type with private fields and chainable `const fn` setters, so a
/// configuration can be a `const`:
///
/// ```
/// use oneyaml_core::{Limits, Settings};
/// const CFG: Settings = Settings::new().merge_keys(true).max_depth(512);
/// const UNTRUSTED: Settings = Settings::new().limits(Limits::untrusted());
/// # assert!(CFG.get_merge_keys());
/// ```
///
/// Settings are always passed explicitly; there are no globals and no
/// environment variables. Every departure from YAML 1.2 is off by default.
///
/// # Settings, by the layer that consumes them
///
/// * **`merge_keys`** — YAML 1.1 merge keys (`<<`). *Resolver / `Value` /
///   serde* (later unit).
/// * Other YAML 1.1 behaviours: **`yaml11_booleans`**, **`legacy_octal`**,
///   **`sexagesimal`**, **`digit_separators`**, **`yaml11_int_forms`**.
///   *Resolver*.
/// * **`schema`** — Core (default), JSON, or Failsafe. *Resolver* (later
///   unit).
/// * **`duplicate_keys`** — *tree builder and serde* (later unit).
/// * **`honor_version_directive`** — `%YAML 1.1` switches that document to
///   the [`Compat::yaml_1_1()`] behaviours. *Resolver* (later unit); the
///   parser reports the directive either way.
/// * **`lenient_inline_colons`** — *parser* (consumed now).
/// * **`resolve_tagged_variant_content`** — *serde deserializer* (typed
///   enum reads of `!Name value`, also through `from_value_with` and
///   `NodeRef::deserialize`) and *serializer* ([`Layout::SerdeYaml`]).
/// * **`limits`** ([`Limits`]) — `max_depth`, `max_scalar_len`,
///   `max_document_size`, and `max_documents` are enforced by the *parser*
///   now; `alias_budget` and `alias_byte_budget` by `Value`/serde reads.
/// * **`emit`** ([`Emit`]) — *serializer*. The serializer also enforces
///   `max_depth` on `Value`s and `max_typed_depth` on Rust types, and
///   refuses the JSON and Failsafe schemas (its output is written for
///   Core).
///
/// # Examples
///
/// Every setter has a getter of the same name with `get_`:
///
/// ```
/// use oneyaml_core::{DuplicateKeys, Emit, Limits, Schema, Settings};
///
/// // A YAML 1.1 reader for one legacy file, spelled out by hand.
/// let s = Settings::new()
///     .merge_keys(true)
///     .yaml11_booleans(true)
///     .legacy_octal(true)
///     .sexagesimal(true)
///     .digit_separators(true)
///     .yaml11_int_forms(true)
///     .schema(Schema::Core)
///     .duplicate_keys(DuplicateKeys::LastWins)
///     .honor_version_directive(true)
///     .lenient_inline_colons(false)
///     .resolve_tagged_variant_content(false)
///     .limits(Limits::untrusted())
///     .max_depth(64)
///     .emit(Emit::new())
///     .indent(4);
/// assert!(s.get_merge_keys() && s.get_yaml11_booleans() && s.get_legacy_octal());
/// assert!(s.get_sexagesimal() && s.get_digit_separators() && s.get_yaml11_int_forms());
/// assert_eq!(s.get_schema(), Schema::Core);
/// assert_eq!(s.get_duplicate_keys(), DuplicateKeys::LastWins);
/// assert!(s.get_honor_version_directive());
/// assert!(!s.get_lenient_inline_colons() && !s.get_resolve_tagged_variant_content());
/// // `max_depth` and `indent` are shortcuts into `limits` and `emit`.
/// assert_eq!(s.get_limits().get_max_depth(), 64);
/// assert_eq!(s.get_emit().get_indent(), 4);
/// // The same 1.1 behaviours as a preset (limits stay the defaults).
/// let preset = oneyaml_core::Compat::yaml_1_1();
/// assert!(preset.get_yaml11_booleans() && preset.get_limits() == &Limits::default());
/// ```
// One value type; Clone, Debug, PartialEq, Eq, Hash.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Settings {
    merge_keys: bool,
    yaml11_booleans: bool,
    legacy_octal: bool,
    sexagesimal: bool,
    digit_separators: bool,
    yaml11_int_forms: bool,
    schema: Schema,
    duplicate_keys: DuplicateKeys,
    honor_version_directive: bool,
    lenient_inline_colons: bool,
    resolve_tagged_variant_content: bool,
    limits: Limits,
    emit: Emit,
}

impl Settings {
    /// YAML 1.2, Core schema, default limits, default output. Same as
    /// [`Settings::default()`], usable in `const`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            merge_keys: false,
            yaml11_booleans: false,
            legacy_octal: false,
            sexagesimal: false,
            digit_separators: false,
            yaml11_int_forms: false,
            schema: Schema::Core,
            duplicate_keys: DuplicateKeys::Error,
            honor_version_directive: false,
            lenient_inline_colons: false,
            resolve_tagged_variant_content: false,
            limits: Limits::new(),
            emit: Emit::new(),
        }
    }

    /// **YAML 1.1 merge keys (`<<`)**, e.g. for docker-compose files and
    /// GitLab CI. Off by default. Merges through aliases count toward
    /// [`Limits::alias_budget`].
    ///
    /// YAML 1.2 has no merge keys: without this setting `<<` is an
    /// ordinary string key, as the spec says. [`Compat::yaml_1_1()`] turns
    /// it on with every other YAML 1.1 behaviour; [`Compat::serde_yaml()`]
    /// leaves it off, as `serde_yaml`'s deserializer does. A `Document`
    /// keeps `<<` entries as written and edits them like any other entry.
    #[must_use]
    pub const fn merge_keys(mut self, on: bool) -> Self {
        self.merge_keys = on;
        self
    }

    /// YAML 1.1 booleans (`yes`/`no`/`on`/`off`/`y`/`n`, …). Off by default.
    #[must_use]
    pub const fn yaml11_booleans(mut self, on: bool) -> Self {
        self.yaml11_booleans = on;
        self
    }

    /// YAML 1.1 legacy octal integers (`0777`). Off by default.
    #[must_use]
    pub const fn legacy_octal(mut self, on: bool) -> Self {
        self.legacy_octal = on;
        self
    }

    /// YAML 1.1 sexagesimal (base-60) numbers (`12:30`). Off by default.
    #[must_use]
    pub const fn sexagesimal(mut self, on: bool) -> Self {
        self.sexagesimal = on;
        self
    }

    /// YAML 1.1 digit separators (`1_000`). Off by default.
    #[must_use]
    pub const fn digit_separators(mut self, on: bool) -> Self {
        self.digit_separators = on;
        self
    }

    /// YAML 1.1 integer forms that YAML 1.2 dropped: binary (`0b1010`,
    /// optionally signed) and a sign on hexadecimal (`-0x1F`, `+0x1f`).
    /// Off by default. `_` separators in these forms still need
    /// [`Settings::digit_separators`]; 1.1 octal, signed or not (`017`,
    /// `-017`), is [`Settings::legacy_octal`]. A signed `0o` (`-0o17`) is
    /// neither YAML 1.1 nor 1.2 and stays a string under every setting.
    #[must_use]
    pub const fn yaml11_int_forms(mut self, on: bool) -> Self {
        self.yaml11_int_forms = on;
        self
    }

    /// The schema (default [`Schema::Core`]).
    #[must_use]
    pub const fn schema(mut self, schema: Schema) -> Self {
        self.schema = schema;
        self
    }

    /// Duplicate-key handling (default [`DuplicateKeys::Error`]).
    #[must_use]
    pub const fn duplicate_keys(mut self, policy: DuplicateKeys) -> Self {
        self.duplicate_keys = policy;
        self
    }

    /// Let `%YAML 1.1` switch its document to the YAML 1.1 behaviours of
    /// [`Compat::yaml_1_1()`]; `%YAML 1.0` gets the same preset, 1.1 being
    /// the closest supported version. Off by default: such documents are
    /// read as YAML 1.2 with a warning.
    #[must_use]
    pub const fn honor_version_directive(mut self, on: bool) -> Self {
        self.honor_version_directive = on;
        self
    }

    /// Leniency, not YAML: read a same-line `: ` inside an unquoted block
    /// value as text instead of an error, e.g. `title: The farm: a banana
    /// story` becomes the string `The farm: a banana story`. Off by
    /// default; when it applies, a warning is **always** recorded. Flow
    /// context is unaffected, and no valid input changes meaning. Not part
    /// of any [`Compat`] preset.
    #[must_use]
    pub const fn lenient_inline_colons(mut self, on: bool) -> Self {
        self.lenient_inline_colons = on;
        self
    }

    /// `serde_yaml` compatibility, not YAML 1.2: when a **typed enum** reads
    /// a locally tagged plain scalar (`!Name value`) as its variant `Name`,
    /// the content is resolved as an untagged plain scalar under the Core
    /// schema (with the document's YAML 1.1 integer, boolean, … settings),
    /// so `!Square 2.0` reads as `Square(2.0)` and `!Enabled true` as
    /// `Enabled(true)`. This is how `serde_yaml` writes a newtype variant
    /// around a number, bool, or null; with the setting on,
    /// [`Layout::SerdeYaml`] writes such variants that way too.
    ///
    /// Off by default: YAML 1.2 leaves the meaning of a local tag to the
    /// application (§6.9.2), and without the setting a custom-tagged scalar
    /// is the tagged *string*. On in [`Compat::serde_yaml()`] only.
    /// Only typed enum reads change: `Value` keeps
    /// `Tagged("!Square", String("2.0"))`, untyped reads (`deserialize_any`)
    /// are unchanged, and quoted content stays a string (`!Square '2.0'` is
    /// text; `from_value_with` cannot tell, as a `Value` keeps no style, and
    /// there resolves to a bool or a number only, never to null).
    /// Global and `%TAG`-expanded tags are not local and never resolve.
    ///
    /// Under this setting (with [`Layout::SerdeYaml`]) the streaming
    /// serializer (`to_string_with` and friends) writes such a variant as
    /// `!Name v`, while `to_value_with` followed by emitting the `Value`
    /// writes `Name: v`: a `Value` has no enums, and its tagged scalars are
    /// strings, so the meaning-level map form is the only `Value` that reads
    /// back as the variant.
    #[must_use]
    pub const fn resolve_tagged_variant_content(mut self, on: bool) -> Self {
        self.resolve_tagged_variant_content = on;
        self
    }

    /// Replaces the resource limits (default [`Limits::default()`]).
    #[must_use]
    pub const fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Shortcut for [`Limits::max_depth`] on the current limits.
    #[must_use]
    pub const fn max_depth(mut self, depth: usize) -> Self {
        self.limits = self.limits.max_depth(depth);
        self
    }

    /// Replaces the output settings (default [`Emit::default()`]).
    #[must_use]
    pub const fn emit(mut self, emit: Emit) -> Self {
        self.emit = emit;
        self
    }

    /// Shortcut for [`Emit::indent`] on the current output settings.
    #[must_use]
    pub const fn indent(mut self, spaces: usize) -> Self {
        self.emit = self.emit.indent(spaces);
        self
    }

    /// Whether merge keys are on.
    #[must_use]
    pub const fn get_merge_keys(&self) -> bool {
        self.merge_keys
    }

    /// Whether YAML 1.1 booleans are on.
    #[must_use]
    pub const fn get_yaml11_booleans(&self) -> bool {
        self.yaml11_booleans
    }

    /// Whether YAML 1.1 legacy octal is on.
    #[must_use]
    pub const fn get_legacy_octal(&self) -> bool {
        self.legacy_octal
    }

    /// Whether YAML 1.1 sexagesimal numbers are on.
    #[must_use]
    pub const fn get_sexagesimal(&self) -> bool {
        self.sexagesimal
    }

    /// Whether YAML 1.1 digit separators are on.
    #[must_use]
    pub const fn get_digit_separators(&self) -> bool {
        self.digit_separators
    }

    /// Whether the YAML 1.1 integer forms (binary, signed hex/octal) are on.
    #[must_use]
    pub const fn get_yaml11_int_forms(&self) -> bool {
        self.yaml11_int_forms
    }

    /// The schema.
    #[must_use]
    pub const fn get_schema(&self) -> Schema {
        self.schema
    }

    /// The duplicate-key policy.
    #[must_use]
    pub const fn get_duplicate_keys(&self) -> DuplicateKeys {
        self.duplicate_keys
    }

    /// Whether `%YAML 1.1` switches to the YAML 1.1 behaviours.
    #[must_use]
    pub const fn get_honor_version_directive(&self) -> bool {
        self.honor_version_directive
    }

    /// Whether inline colons in unquoted block values are read as text.
    #[must_use]
    pub const fn get_lenient_inline_colons(&self) -> bool {
        self.lenient_inline_colons
    }

    /// Whether typed enum reads resolve a locally tagged plain scalar's
    /// content (`!Square 2.0` → `Square(2.0)`).
    #[must_use]
    pub const fn get_resolve_tagged_variant_content(&self) -> bool {
        self.resolve_tagged_variant_content
    }

    /// The resource limits.
    #[must_use]
    pub const fn get_limits(&self) -> &Limits {
        &self.limits
    }

    /// The output settings.
    #[must_use]
    pub const fn get_emit(&self) -> &Emit {
        &self.emit
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

/// Compatibility presets. Each returns a [`Settings`] that can be
/// refined further; presets never change [`Limits`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct Compat;

impl Compat {
    /// Behave like `serde_yaml` for migration: duplicate keys are tolerated
    /// (last wins, with a warning), the YAML 1.1 integer forms `serde_yaml`
    /// reads as integers are integers ([`Settings::yaml11_int_forms`]:
    /// `0b1010`, `-0x10`, `+0x10`), and output follows `serde_yaml`'s layout
    /// (non-indented sequences, [`Layout::SerdeYaml`]), and typed enums
    /// read and write newtype variants around numbers, bools, and nulls as
    /// tagged scalars (`!Square 2.0`,
    /// [`Settings::resolve_tagged_variant_content`]).
    ///
    /// The remaining reading differences from `serde_yaml` are listed, with
    /// their reasons, in `docs/migrating-from-serde-yaml.md`.
    ///
    /// A preset is a starting point: refine it like any [`Settings`], here
    /// for a `docker-compose.yml` that relies on merge keys:
    ///
    /// ```
    /// use oneyaml_core::{Compat, DuplicateKeys, Layout, Settings};
    ///
    /// const COMPOSE: Settings = Compat::serde_yaml().merge_keys(true);
    ///
    /// assert_eq!(COMPOSE.get_duplicate_keys(), DuplicateKeys::LastWins);
    /// assert!(COMPOSE.get_yaml11_int_forms() && COMPOSE.get_merge_keys());
    /// assert_eq!(COMPOSE.get_emit().get_layout(), Layout::SerdeYaml);
    /// // Presets never change limits.
    /// assert_eq!(COMPOSE.get_limits(), Settings::new().get_limits());
    /// ```
    #[must_use]
    pub const fn serde_yaml() -> Settings {
        Settings::new()
            .duplicate_keys(DuplicateKeys::LastWins)
            // `serde_yaml` reads `0b…` and signed `0x…` as integers
            // (the compat differential); an existing named setting.
            .yaml11_int_forms(true)
            // `!Square 2.0` reads into
            // `Square(2.0)` in typed enums, and is written that way.
            .resolve_tagged_variant_content(true)
            .emit(
                Emit::new()
                    .indent_sequences(false)
                    .layout(Layout::SerdeYaml),
            )
    }

    /// Full YAML 1.1 typing: merge keys, 1.1 booleans, legacy octal,
    /// sexagesimal numbers, digit separators, and the 1.1 integer forms.
    #[must_use]
    pub const fn yaml_1_1() -> Settings {
        Settings::new()
            .merge_keys(true)
            .yaml11_booleans(true)
            .legacy_octal(true)
            .sexagesimal(true)
            .digit_separators(true)
            .yaml11_int_forms(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layout, QuoteStyle};

    /// Tripwire: `Schema` is `#[non_exhaustive]`, so downstream
    /// matches need a wildcard. This exhaustive match (legal only inside
    /// this crate) stops compiling when a variant is added: give it a
    /// resolver in `oneyaml`'s `Resolver::for_document` first, whose
    /// wildcard otherwise reports every node as Y9000.
    #[test]
    fn schema_variants_are_all_resolved() {
        for s in [Schema::Core, Schema::Json, Schema::Failsafe] {
            match s {
                Schema::Core | Schema::Json | Schema::Failsafe => {}
            }
        }
    }

    /// Tripwire, as for `Schema`: handle a new `DuplicateKeys`
    /// variant in `oneyaml`'s `on_duplicate` first, whose wildcard
    /// otherwise fails every duplicate with Y9000.
    #[test]
    fn duplicate_keys_variants_are_all_handled() {
        for p in [DuplicateKeys::Error, DuplicateKeys::LastWins] {
            match p {
                DuplicateKeys::Error | DuplicateKeys::LastWins => {}
            }
        }
    }

    #[test]
    fn defaults_are_yaml_1_2() {
        let s = Settings::default();
        assert_eq!(s, Settings::new());
        assert!(!s.get_merge_keys());
        assert!(!s.get_yaml11_booleans());
        assert!(!s.get_legacy_octal());
        assert!(!s.get_sexagesimal());
        assert!(!s.get_digit_separators());
        assert!(!s.get_yaml11_int_forms());
        assert_eq!(s.get_schema(), Schema::Core);
        assert_eq!(s.get_duplicate_keys(), DuplicateKeys::Error);
        assert!(!s.get_honor_version_directive());
        assert!(!s.get_lenient_inline_colons());
        assert!(!s.get_resolve_tagged_variant_content());
        assert_eq!(s.get_limits(), &Limits::default());
        assert_eq!(s.get_emit(), &Emit::default());
    }

    #[test]
    fn every_setter_chains_in_const() {
        const S: Settings = Settings::new()
            .merge_keys(true)
            .yaml11_booleans(true)
            .legacy_octal(true)
            .sexagesimal(true)
            .digit_separators(true)
            .yaml11_int_forms(true)
            .schema(Schema::Failsafe)
            .duplicate_keys(DuplicateKeys::LastWins)
            .honor_version_directive(true)
            .lenient_inline_colons(true)
            .resolve_tagged_variant_content(true)
            .limits(Limits::untrusted())
            .max_depth(5)
            .emit(Emit::new().quote_style(QuoteStyle::Double))
            .indent(3);
        assert!(S.get_merge_keys() && S.get_yaml11_booleans() && S.get_legacy_octal());
        assert!(S.get_sexagesimal() && S.get_digit_separators() && S.get_yaml11_int_forms());
        assert!(S.get_honor_version_directive() && S.get_lenient_inline_colons());
        assert!(S.get_resolve_tagged_variant_content());
        assert_eq!(S.get_schema(), Schema::Failsafe);
        assert_eq!(S.get_duplicate_keys(), DuplicateKeys::LastWins);
        assert_eq!(S.get_limits(), &Limits::untrusted().max_depth(5));
        assert_eq!(
            S.get_emit(),
            &Emit::new().quote_style(QuoteStyle::Double).indent(3)
        );
        assert_eq!(Schema::default(), Schema::Core);
        assert_eq!(DuplicateKeys::default(), DuplicateKeys::Error);
    }

    #[test]
    fn presets() {
        let s = Compat::serde_yaml();
        assert_eq!(s.get_duplicate_keys(), DuplicateKeys::LastWins);
        assert_eq!(s.get_emit().get_layout(), Layout::SerdeYaml);
        assert!(!s.get_emit().get_indent_sequences());
        assert!(!s.get_merge_keys() && !s.get_lenient_inline_colons());
        assert!(s.get_yaml11_int_forms() && s.get_resolve_tagged_variant_content());
        assert!(!s.get_yaml11_booleans() && !s.get_legacy_octal());
        assert!(!s.get_sexagesimal() && !s.get_digit_separators());

        let y = Compat::yaml_1_1();
        assert!(y.get_merge_keys() && y.get_yaml11_booleans() && y.get_legacy_octal());
        assert!(y.get_sexagesimal() && y.get_digit_separators() && y.get_yaml11_int_forms());
        assert!(!y.get_lenient_inline_colons() && !y.get_resolve_tagged_variant_content());
        assert_eq!(y.get_schema(), Schema::Core);

        // Presets never change limits.
        for preset in [s, y] {
            assert_eq!(preset.get_limits(), &Limits::default());
        }
        // Overrides chain on presets.
        let o = Compat::serde_yaml().duplicate_keys(DuplicateKeys::Error);
        assert_eq!(o.get_duplicate_keys(), DuplicateKeys::Error);
    }
}
