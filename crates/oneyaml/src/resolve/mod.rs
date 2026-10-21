//! The one sealed resolver.
//!
//! Maps a node's (tag, style, text) to its meaning under the active schema
//! and settings. The tree builder and the serde deserializer both call
//! [`Resolver`]; neither decides the meaning of a scalar on its own, so the
//! two layers cannot disagree. The trait is internal and sealed until
//! post-1.0.
//!
//! Inputs are plain data (tag text, a two-way style, scalar text) so the
//! resolver does not depend on the event accessor API.

// Some items are used by one consumer only.
#![allow(dead_code, reason = "consumed by the tree builder and serde layer")]

pub(crate) mod conflict;
mod scan;

/// The bool YAML 1.1 gave `text` when it is one of 1.1's extra bool words
/// (`y`, `yes`, `on`, `n`, `no`, `off` in their 1.1 casings), which YAML
/// 1.2's Core schema reads as strings.
pub(crate) fn yaml11_bool_word(text: &str) -> Option<bool> {
    scan::yaml11_only_bool(text)
}
#[cfg(test)]
mod tests;

use oneyaml_core::{Schema, Settings};

use crate::value::{Number, TagStr};
use scan::Ext;

/// The `tag:yaml.org,2002:` prefix (§10).
const YAML_PREFIX: &str = "tag:yaml.org,2002:";
/// YAML 1.1 merge key tag (yaml.org/type/merge).
pub(crate) const MERGE_TAG: &str = "tag:yaml.org,2002:merge";

/// A node's tag as the resolver sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TagInput<'t> {
    /// No tag property: the `?` non-specific tag for plain scalars, `!` for
    /// everything else (§6.9.1.3).
    Absent,
    /// The explicit non-specific tag `!`.
    NonSpecific,
    /// A specific tag, fully resolved (after `%TAG` expansion), e.g.
    /// `tag:yaml.org,2002:int` or `!local`.
    Named(TagStr<'t>),
}

/// Whether a scalar was plain. Only plain, untagged scalars are resolved by
/// the schema's regexes; every other style is a string unless tagged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    /// Unquoted flow scalar.
    Plain,
    /// Single- or double-quoted, literal, or folded.
    NonPlain,
}

/// A tag in the `tag:yaml.org,2002:` namespace that some built-in schema
/// knows (§10.1–10.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoreTag {
    Null,
    Bool,
    Int,
    Float,
    Str,
    Seq,
    Map,
}

impl CoreTag {
    /// O(1): a core tag is short (a long tag is never copied).
    fn parse(tag: TagStr<'_>) -> Option<Self> {
        if tag.len() > YAML_PREFIX.len().saturating_add(5) {
            return None;
        }
        let rest = tag.strip_prefix(YAML_PREFIX)?;
        let mut name = [0u8; 5];
        let mut n = 0usize;
        for b in rest.bytes() {
            *name.get_mut(n)? = b;
            n = n.saturating_add(1);
        }
        Some(match name.get(..n)? {
            b"null" => Self::Null,
            b"bool" => Self::Bool,
            b"int" => Self::Int,
            b"float" => Self::Float,
            b"str" => Self::Str,
            b"seq" => Self::Seq,
            b"map" => Self::Map,
            _ => return None,
        })
    }
}

/// The meaning of a scalar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Resolved<'t> {
    Null,
    Bool(bool),
    /// An integer (any width) or a float; see [`Number`].
    Number(Number),
    /// A string: the scalar's content, as the caller already holds it.
    Str,
    /// A tag this schema does not resolve; the content stays a string (a
    /// custom tag is no licence to read `!x 12` as a number).
    Custom(TagStr<'t>),
}

/// The meaning of a collection's tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedCollection<'t> {
    /// Untagged, `!`, or the matching `!!seq` / `!!map`.
    Plain,
    /// A tag this schema does not resolve.
    Custom(TagStr<'t>),
}

/// A collection kind, for [`Resolver::resolve_collection`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollectionKind {
    Sequence,
    Mapping,
}

/// Why a node could not be resolved. Each needs a Y1xxx code (allocated by
/// the code registry in `oneyaml-core`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// An explicit `!!null`/`!!bool`/`!!int`/`!!float` whose text is not a
    /// valid form of that type under the schema (e.g. `!!int twelve`).
    InvalidForTag(CoreTag),
    /// `!!seq` or `!!map` on a scalar.
    CollectionTagOnScalar(CoreTag),
    /// A scalar tag (or the other collection tag) on a collection, e.g.
    /// `!!str [a]` or `!!map [a]`.
    WrongTagOnCollection(CoreTag),
    /// JSON schema (§10.2.2): a plain scalar matching none of the JSON
    /// regexes is an error.
    JsonUnmatched,
    /// An internal invariant failed (release-build outcome of a
    /// `debug_assert!`; `Y9000`).
    Internal,
}

mod sealed {
    pub trait Sealed {}
}

/// One schema's tag resolution. Sealed: only the built-in schemas below
/// implement it.
pub(crate) trait ResolveSchema: sealed::Sealed {
    /// Tag resolution of an untagged plain scalar (the `?` tag).
    fn plain(&self, text: &str) -> Result<Resolved<'static>, ResolveError>;
    /// Whether this schema recognises `tag`.
    fn recognises(&self, tag: CoreTag) -> bool;
    /// Constructs a recognised scalar tag's value from `text`.
    fn construct(&self, tag: CoreTag, text: &str) -> Result<Resolved<'static>, ResolveError>;
}

/// YAML 1.2 Core schema (§10.3), extended by the enabled 1.1 behaviours.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Core {
    ext: Ext,
}

/// YAML 1.2 JSON schema (§10.2).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Json;

/// YAML 1.2 Failsafe schema (§10.1).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Failsafe;

impl sealed::Sealed for Core {}
impl sealed::Sealed for Json {}
impl sealed::Sealed for Failsafe {}

type Outcome = Result<Resolved<'static>, ResolveError>;

fn num(n: Option<Number>) -> Option<Outcome> {
    n.map(|n| Ok(Resolved::Number(n)))
}

fn float(f: Option<Result<Number, ResolveError>>) -> Option<Outcome> {
    f.map(|f| f.map(Resolved::Number))
}

fn ok(r: Option<Resolved<'static>>) -> Option<Outcome> {
    r.map(Ok)
}

impl ResolveSchema for Core {
    // §10.3.2 tag resolution, in the table's order.
    fn plain(&self, t: &str) -> Result<Resolved<'static>, ResolveError> {
        if scan::core_null(t) {
            return Ok(Resolved::Null);
        }
        if let Some(b) = scan::ext_bool(t, self.ext) {
            return Ok(Resolved::Bool(b));
        }
        num(scan::core_int(t, self.ext))
            .or_else(|| float(scan::core_float(t, self.ext)))
            .unwrap_or(Ok(Resolved::Str))
    }

    fn recognises(&self, _: CoreTag) -> bool {
        true
    }

    fn construct(&self, tag: CoreTag, t: &str) -> Result<Resolved<'static>, ResolveError> {
        let r = match tag {
            CoreTag::Null => ok(scan::core_null(t).then_some(Resolved::Null)),
            CoreTag::Bool => ok(scan::ext_bool(t, self.ext).map(Resolved::Bool)),
            CoreTag::Int => num(scan::core_int(t, self.ext)),
            CoreTag::Float => float(scan::core_float(t, self.ext)),
            CoreTag::Str => Some(Ok(Resolved::Str)),
            CoreTag::Seq | CoreTag::Map => {
                return Err(ResolveError::CollectionTagOnScalar(tag));
            }
        };
        r.unwrap_or(Err(ResolveError::InvalidForTag(tag)))
    }
}

impl ResolveSchema for Json {
    // §10.2.2 tag resolution; the final row (`.*`) is an error.
    fn plain(&self, t: &str) -> Result<Resolved<'static>, ResolveError> {
        if scan::json_null(t) {
            return Ok(Resolved::Null);
        }
        if let Some(b) = scan::json_bool(t) {
            return Ok(Resolved::Bool(b));
        }
        num(scan::json_int(t))
            .or_else(|| float(scan::json_float(t)))
            .unwrap_or(Err(ResolveError::JsonUnmatched))
    }

    fn recognises(&self, _: CoreTag) -> bool {
        true
    }

    fn construct(&self, tag: CoreTag, t: &str) -> Result<Resolved<'static>, ResolveError> {
        let r = match tag {
            CoreTag::Null => ok(scan::json_null(t).then_some(Resolved::Null)),
            CoreTag::Bool => ok(scan::json_bool(t).map(Resolved::Bool)),
            CoreTag::Int => num(scan::json_int(t)),
            CoreTag::Float => float(scan::json_float(t)),
            CoreTag::Str => Some(Ok(Resolved::Str)),
            CoreTag::Seq | CoreTag::Map => {
                return Err(ResolveError::CollectionTagOnScalar(tag));
            }
        };
        r.unwrap_or(Err(ResolveError::InvalidForTag(tag)))
    }
}

impl ResolveSchema for Failsafe {
    // §10.1: every scalar is a string; only str, seq, and map are known.
    fn plain(&self, _: &str) -> Result<Resolved<'static>, ResolveError> {
        Ok(Resolved::Str)
    }

    fn recognises(&self, tag: CoreTag) -> bool {
        matches!(tag, CoreTag::Str | CoreTag::Seq | CoreTag::Map)
    }

    fn construct(&self, tag: CoreTag, _: &str) -> Result<Resolved<'static>, ResolveError> {
        match tag {
            CoreTag::Str => Ok(Resolved::Str),
            CoreTag::Seq | CoreTag::Map => Err(ResolveError::CollectionTagOnScalar(tag)),
            // Not recognised by Failsafe; `resolve_scalar` never asks.
            _ => {
                debug_assert!(false, "failsafe asked to construct {tag:?}");
                Err(ResolveError::Internal)
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Active {
    Core(Core),
    Json(Json),
    Failsafe(Failsafe),
    /// A `Schema` this version has no resolver for (see `for_document`).
    Unsupported,
}

/// The resolver for one document: the active schema plus the settings that
/// shape node meaning. Build it once per document and share it between
/// consumers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolver {
    active: Active,
    merge_keys: bool,
    /// `Settings::resolve_tagged_variant_content`: the Core schema (with
    /// this document's 1.1 extensions) that reads a locally tagged plain
    /// scalar's content in a typed enum; `None` when off.
    variant_content: Option<Core>,
}

impl Resolver {
    /// The resolver for `settings`, ignoring any `%YAML` directive.
    pub(crate) fn new(settings: &Settings) -> Self {
        Self::for_document(settings, false)
    }

    /// The resolver for one document. `yaml_1_1_directive`: the document
    /// carries `%YAML 1.1`. With `honor_version_directive` on, that turns
    /// on all YAML 1.1 behaviours of `Compat::yaml_1_1()` for this document
    /// (as YAML 1.1 documents ask); the schema and limits are unchanged.
    pub(crate) fn for_document(settings: &Settings, yaml_1_1_directive: bool) -> Self {
        let switch = yaml_1_1_directive && settings.get_honor_version_directive();
        let ext = Ext {
            booleans: switch || settings.get_yaml11_booleans(),
            legacy_octal: switch || settings.get_legacy_octal(),
            sexagesimal: switch || settings.get_sexagesimal(),
            separators: switch || settings.get_digit_separators(),
            int_forms: switch || settings.get_yaml11_int_forms(),
        };
        let active = match settings.get_schema() {
            Schema::Core => Active::Core(Core { ext }),
            Schema::Json => Active::Json(Json),
            Schema::Failsafe => Active::Failsafe(Failsafe),
            // `Schema` is `#[non_exhaustive]` in `-core`, so a wildcard is
            // required here. A schema added there without a resolver here
            // must never be read as another schema: every node of such a
            // document is a Y9000 error instead. (`-core`'s
            // `schema_variants_are_all_resolved` test trips on a new
            // variant first.)
            _ => {
                debug_assert!(false, "no resolver for {:?}", settings.get_schema());
                Active::Unsupported
            }
        };
        Self {
            active,
            merge_keys: switch || settings.get_merge_keys(),
            variant_content: settings
                .get_resolve_tagged_variant_content()
                .then_some(Core { ext }),
        }
    }

    /// The active schema; `Err(Internal)` (Y9000) for an unsupported one.
    fn schema(&self) -> Result<&dyn ResolveSchema, ResolveError> {
        match &self.active {
            Active::Core(s) => Ok(s),
            Active::Json(s) => Ok(s),
            Active::Failsafe(s) => Ok(s),
            Active::Unsupported => Err(ResolveError::Internal),
        }
    }

    /// The meaning of a scalar node (§10, §6.9.1.3).
    ///
    /// * Untagged plain: the schema's tag resolution.
    /// * Untagged non-plain, or `!`: a string.
    /// * A recognised `tag:yaml.org,2002:` tag: that type, whatever the
    ///   style; the text must be a valid form of it under the schema.
    /// * Any other tag: [`Resolved::Custom`].
    pub(crate) fn resolve_scalar<'t>(
        &self,
        tag: TagInput<'t>,
        style: Style,
        text: &str,
    ) -> Result<Resolved<'t>, ResolveError> {
        let schema = self.schema()?;
        match tag {
            TagInput::Absent => match style {
                Style::Plain => schema.plain(text),
                Style::NonPlain => Ok(Resolved::Str),
            },
            TagInput::NonSpecific => Ok(Resolved::Str),
            TagInput::Named(t) => match CoreTag::parse(t) {
                Some(ct) if schema.recognises(ct) => schema.construct(ct, text),
                _ => Ok(Resolved::Custom(t)),
            },
        }
    }

    /// The meaning of a collection's tag.
    pub(crate) fn resolve_collection<'t>(
        &self,
        tag: TagInput<'t>,
        kind: CollectionKind,
    ) -> Result<ResolvedCollection<'t>, ResolveError> {
        let schema = self.schema()?;
        let TagInput::Named(t) = tag else {
            return Ok(ResolvedCollection::Plain);
        };
        match CoreTag::parse(t) {
            Some(ct) if schema.recognises(ct) => match (ct, kind) {
                (CoreTag::Seq, CollectionKind::Sequence)
                | (CoreTag::Map, CollectionKind::Mapping) => Ok(ResolvedCollection::Plain),
                _ => Err(ResolveError::WrongTagOnCollection(ct)),
            },
            _ => Ok(ResolvedCollection::Custom(t)),
        }
    }

    /// Mapping-construction hook for YAML 1.1 merge keys: true when
    /// `merge_keys` is on and this key node is a merge key — the plain,
    /// untagged `<<`, or any scalar explicitly tagged `!!merge`. The tree
    /// builder / deserializer then merges the value (a mapping or a
    /// sequence of mappings, counting toward the alias budget)
    /// instead of inserting a `<<` entry. When off, `<<` is an ordinary
    /// string key and `!!merge` is a custom tag.
    pub(crate) fn is_merge_key(&self, tag: TagInput<'_>, style: Style, text: &str) -> bool {
        self.merge_keys
            && match tag {
                TagInput::Absent => style == Style::Plain && text == "<<",
                TagInput::Named(t) => t.is(MERGE_TAG),
                TagInput::NonSpecific => false,
            }
    }

    /// The meaning of a locally tagged (`!Name`) plain scalar's content
    /// when a typed enum reads it as variant `Name`, under
    /// `Settings::resolve_tagged_variant_content`: the Core schema's tag
    /// resolution of `text` as an untagged plain scalar (with the
    /// document's YAML 1.1 extensions). `None` when the setting is off, the
    /// scalar was not plain, or the tag is not local (a global or
    /// `%TAG`-expanded tag names no variant of the application's;
    /// YAML 1.2.2 §6.9.2 leaves local tags to the application).
    ///
    /// An opt-in, named departure; the node's own meaning (its
    /// `Value`, untyped reads) stays the tagged string.
    ///
    /// # Errors
    ///
    /// `Internal` (Y9000) when Core's float scan breaks its own
    /// invariant; Core's tag resolution has no other error (its last row is
    /// `str`).
    pub(crate) fn variant_content(
        &self,
        tag: TagStr<'_>,
        style: Style,
        text: &str,
    ) -> Result<Option<Resolved<'static>>, ResolveError> {
        match self.variant_content {
            Some(core) if self.resolves_variant_content(tag, style) => core.plain(text).map(Some),
            _ => Ok(None),
        }
    }

    /// Whether [`Resolver::variant_content`] would resolve the content of a
    /// scalar with `tag` and `style` (the setting is on, the scalar is
    /// plain, the tag is local). No text is read: readers keep this bit and
    /// resolve only when a typed enum reads the content.
    pub(crate) fn resolves_variant_content(&self, tag: TagStr<'_>, style: Style) -> bool {
        self.variant_content.is_some()
            && style == Style::Plain
            && tag.len() > 1
            && tag.starts_with("!")
    }

    /// Whether merge keys are on for this document.
    pub(crate) fn merge_keys(&self) -> bool {
        self.merge_keys
    }

    /// Whether untagged plain scalars resolve exactly as under the Core
    /// schema with no YAML 1.1 behaviour (`Resolver::new(&Settings::new())`):
    /// the schema is Core and no 1.1 extension is on. Merge keys do not
    /// change scalar resolution.
    pub(crate) fn plain_is_core(&self) -> bool {
        matches!(self.active, Active::Core(Core { ext }) if ext == Ext::default())
    }

    /// Whether a plain scalar this resolver reads as a string may be one of
    /// YAML 1.1's extra bool words (`yes`, `off`, …; [`yaml11_bool_word`]):
    /// the Core schema is active without `yaml11_booleans`. Only feeds the
    /// help of a type mismatch; it never changes what a node means.
    pub(crate) fn strings_may_be_yaml11_bools(&self) -> bool {
        matches!(self.active, Active::Core(Core { ext }) if !ext.booleans)
    }
}
