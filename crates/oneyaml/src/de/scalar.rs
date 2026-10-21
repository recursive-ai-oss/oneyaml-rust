//! One resolved scalar, delivered to a serde visitor (the coercion rules
//! of the schema and of string targets).
//!
//! * A **plain, untagged** scalar may go into a string target as its text,
//!   whatever it resolves to (`8080` → `"8080"`, `1.10` → `"1.10"`).
//! * A quoted (or `!`/`!!str`-tagged) scalar is a string: never a number
//!   or bool.
//! * Integers go into integer targets only if they fit (else
//!   [`code::INTEGER_OUT_OF_RANGE`]); floats (`.inf`, `.nan`, `1.0`) never.
//! * Bools are only what the schema resolved as bool.
//! * A custom-tagged scalar (`!Name text`) is an enum variant `Name` with
//!   string content; under `Settings::resolve_tagged_variant_content`, a
//!   typed enum reads a *plain* scalar with a *local* tag with its content
//!   resolved under Core (`!Square 2.0` → `Square(2.0)`).

use std::borrow::Cow;

use oneyaml_core::{Span, code};
use serde::de::{self, IntoDeserializer, Unexpected, Visitor};

use super::Hint;
use crate::construct::budget::{ByteMeter, SharedRead};
use crate::construct::scalar::{resolve, resolve_error};
use crate::construct::{Tag, TagText};
use crate::error::Error;
use crate::resolve::{Resolved, Resolver, Style};
use crate::value::{Number, Tag as ValueTag, TagPrefix, TagStr, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

/// What a scalar resolved to.
pub(crate) enum Kind<'de> {
    Null,
    Bool(bool),
    Number(Number),
    Str,
    Custom(TagText<'de>),
}

impl Kind<'_> {
    /// The kind of a scalar the schema resolved (`None` for a custom tag).
    pub(crate) fn plain(r: Resolved<'_>) -> Option<Self> {
        match r {
            Resolved::Null => Some(Kind::Null),
            Resolved::Bool(b) => Some(Kind::Bool(b)),
            Resolved::Number(n) => Some(Kind::Number(n)),
            Resolved::Str => Some(Kind::Str),
            Resolved::Custom(_) => None,
        }
    }
}

/// A resolved scalar.
pub(crate) struct Sc<'de> {
    pub(crate) kind: Kind<'de>,
    pub(crate) value: Cow<'de, str>,
    /// Plain and untagged: may be read as its text by string targets.
    pub(crate) lenient: bool,
    /// A plain string under a schema that reads YAML 1.1's extra bool words
    /// (`yes`, `off`, …) as strings: a bool target's type mismatch on such
    /// a word explains the 1.2 reading. The word itself is only checked on
    /// that error path.
    pub(crate) yaml11_hint: bool,
    /// The resolver for a custom-tagged scalar's content, when a typed enum
    /// would read it resolved (`Settings::resolve_tagged_variant_content`,
    /// see [`Resolver::variant_content`]); `None`: the content is a string.
    /// Resolved only on the typed-enum newtype path.
    pub(crate) variant_content: Option<Resolver>,
    pub(crate) span: Span,
    /// Typed levels open above this node, and `Limits::max_typed_depth`
    /// (bounded by it): options, newtypes, `Spanned`, and variant content
    /// re-enter the same node through the target's own `Deserialize`, so each
    /// re-entry counts a level (`struct R(Option<Box<R>>)` read from one scalar
    /// would otherwise recurse without end).
    pub(crate) depth: usize,
    pub(crate) max: usize,
    /// The read's variant memo, for a custom-tagged scalar a typed enum
    /// may read; `None`: names are matched without one.
    pub(crate) memo: Option<SharedMemo>,
}

impl<'de> Sc<'de> {
    pub(crate) fn classify(
        r: &Resolver,
        tag: Tag<'de>,
        plain: bool,
        value: Cow<'de, str>,
        span: Span,
    ) -> Result<Self, Error> {
        let mut variant_content = None;
        let kind = match resolve(r, &tag, plain, &value, span)? {
            Resolved::Custom(_) => match &tag {
                Tag::Named(t) => {
                    let style = if plain { Style::Plain } else { Style::NonPlain };
                    if r.resolves_variant_content(t.parts(), style) {
                        variant_content = Some(*r);
                    }
                    Kind::Custom(t.clone())
                }
                _ => return Err(Error::internal("custom tag without a name", Some(span))),
            },
            other => match Kind::plain(other) {
                Some(k) => k,
                None => return Err(Error::internal("unclassified scalar", Some(span))),
            },
        };
        let lenient = plain && tag == Tag::Absent;
        let yaml11_hint = lenient && matches!(kind, Kind::Str) && r.strings_may_be_yaml11_bools();
        Ok(Self {
            kind,
            value,
            lenient,
            yaml11_hint,
            variant_content,
            span,
            depth: 0,
            max: usize::MAX,
            memo: None,
        })
    }

    /// With the typed levels of the reader (`Core`'s or `ValueDe`'s).
    pub(crate) fn typed(mut self, depth: usize, max: usize) -> Self {
        self.depth = depth;
        self.max = max;
        self
    }

    /// This scalar, re-entered through the target's `Deserialize` (one
    /// typed level more), or `Y2007` past the limit.
    pub(crate) fn reenter(mut self) -> Result<Self, Error> {
        if self.depth >= self.max {
            return Err(super::core::typed_depth_exceeded(self.max, Some(self.span)));
        }
        self.depth = self.depth.saturating_add(1);
        Ok(self)
    }

    /// The value this scalar denotes (as `Value` would hold it).
    pub(crate) fn to_value(&self) -> Value {
        let text = self.value.clone().into_owned();
        match &self.kind {
            Kind::Null => Value::Null,
            Kind::Bool(b) => Value::Bool(*b),
            Kind::Number(n) => Value::Number(n.clone()),
            Kind::Str => Value::String(text),
            Kind::Custom(t) => Value::Tagged(Box::new(crate::value::TaggedValue {
                tag: t.value_tag(),
                value: Value::String(text),
            })),
        }
    }

    fn unexpected(&self) -> Unexpected<'_> {
        match &self.kind {
            Kind::Null => Unexpected::Unit,
            Kind::Bool(b) => Unexpected::Bool(*b),
            Kind::Number(n) => match (n.as_i64(), n.as_u64(), n.as_float()) {
                (_, Some(u), _) => Unexpected::Unsigned(u),
                (Some(i), _, _) => Unexpected::Signed(i),
                (_, _, Some(f)) => Unexpected::Float(f),
                _ => Unexpected::Other("integer"),
            },
            Kind::Str => Unexpected::Str(&self.value),
            Kind::Custom(_) => Unexpected::Other("tagged scalar"),
        }
    }

    fn mismatch(&self, exp: &dyn de::Expected) -> Error {
        de::Error::invalid_type(self.unexpected(), exp)
    }

    /// The type mismatch of a YAML 1.1 bool word (`yes`, `off`, …) read
    /// into a bool: YAML 1.2 reads it as a string, the most common surprise
    /// for YAML 1.1 users, so the message says why and the help how to fix it.
    fn yaml11_bool_mismatch(&self, meant: bool, exp: &dyn de::Expected) -> Error {
        let word = &*self.value;
        let msg = crate::error::message(format_args!(
            "invalid type: string {word:?}, expected {exp}; YAML 1.2 reads `{word}` as a string (its booleans are `true` and `false`)"
        ));
        let help = format!(
            "write `{meant}` instead of `{word}`, or read the YAML 1.1 booleans (y/n, yes/no, on/off) as booleans with Settings::yaml11_booleans(true) or the Compat::yaml_1_1() preset"
        );
        let mut e = Error::new(code::TYPE_MISMATCH, msg, None).with_help(help);
        // The fix replaces the span with the boolean, so it is offered only
        // when the span is exactly the word. It is not
        // machine-applicable: the rest of the file may rely on the 1.1
        // reading in ways only the author knows.
        if self.span.end.checked_sub(self.span.start) == Some(word.len()) {
            e = e.with_suggestion(oneyaml_core::Suggestion::new(
                format!("write the YAML 1.2 boolean `{meant}`"),
                vec![oneyaml_core::Replacement::new(self.span, meant.to_string())],
                oneyaml_core::Applicability::MaybeIncorrect,
            ));
        }
        e
    }

    fn out_of_range(&self, target: &str) -> Error {
        Error::new(
            code::INTEGER_OUT_OF_RANGE,
            format!("integer {} is out of range for {target}", self.value),
            Some(self.span),
        )
    }

    fn visit_str<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.value {
            Cow::Borrowed(s) => v.visit_borrowed_str(s),
            Cow::Owned(s) => v.visit_string(s),
        }
    }

    /// Delivers the scalar for `hint`.
    pub(crate) fn visit<V: Visitor<'de>>(self, hint: Hint, v: V) -> Result<V::Value, Error> {
        match hint {
            Hint::Any | Hint::Ignored => self.visit_any(v),
            Hint::Bool => match self.kind {
                Kind::Bool(b) => v.visit_bool(b),
                _ => match self
                    .yaml11_hint
                    .then(|| crate::resolve::yaml11_bool_word(&self.value))
                {
                    Some(Some(b)) => Err(self.yaml11_bool_mismatch(b, &v)),
                    _ => Err(self.mismatch(&v)),
                },
            },
            Hint::Int(ty) => self.visit_int(ty, v),
            Hint::Float => match &self.kind {
                Kind::Number(n) if n.is_float() => v.visit_f64(n.as_float().unwrap_or(f64::NAN)),
                Kind::Number(n) => match n.as_f64() {
                    Some(f) => v.visit_f64(f),
                    None => Err(self.out_of_range("f64")),
                },
                _ => Err(self.mismatch(&v)),
            },
            Hint::Str => match self.kind {
                Kind::Str => self.visit_str(v),
                Kind::Null | Kind::Bool(_) | Kind::Number(_) if self.lenient => self.visit_str(v),
                _ => Err(self.mismatch(&v)),
            },
            Hint::Option => match self.kind {
                Kind::Null => v.visit_none(),
                _ => v.visit_some(ScalarDe(self.reenter()?)),
            },
            Hint::Unit => match self.kind {
                Kind::Null => v.visit_unit(),
                _ => Err(self.mismatch(&v)),
            },
            Hint::Newtype => v.visit_newtype_struct(ScalarDe(self.reenter()?)),
            Hint::Enum(names) => match self.kind {
                Kind::Str => v.visit_enum(self.value.into_deserializer()),
                Kind::Null | Kind::Bool(_) | Kind::Number(_) if self.lenient => {
                    v.visit_enum(self.value.into_deserializer())
                }
                Kind::Custom(_) => v.visit_enum(TaggedScalar {
                    sc: self,
                    typed_enum: true,
                    names: Some(names),
                }),
                // Refused as the variant, through the
                // visitor, as `Core` refuses a source scalar it reads as a
                // name (`unknown variant`): a target that ignores the error
                // reads on the same way on every path.
                _ => {
                    let e = self.mismatch(&v);
                    v.visit_enum(RefusedVariant(e))
                }
            },
            Hint::Seq | Hint::Map => Err(self.mismatch(&v)),
        }
    }

    fn visit_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match &self.kind {
            Kind::Null => v.visit_unit(),
            Kind::Bool(b) => v.visit_bool(*b),
            Kind::Number(n) => {
                if let Some(f) = n.as_float() {
                    v.visit_f64(f)
                } else if let Some(u) = n.as_u64() {
                    v.visit_u64(u)
                } else if let Some(i) = n.as_i64() {
                    v.visit_i64(i)
                } else if let Some(u) = n.as_u128() {
                    v.visit_u128(u)
                } else if let Some(i) = n.as_i128() {
                    v.visit_i128(i)
                } else {
                    Err(self.out_of_range("any Rust integer type"))
                }
            }
            Kind::Str => self.visit_str(v),
            // Untyped reads keep the content a string.
            Kind::Custom(_) => v.visit_enum(TaggedScalar {
                sc: self,
                typed_enum: false,
                names: None,
            }),
        }
    }

    fn visit_int<V: Visitor<'de>>(self, ty: IntTy, v: V) -> Result<V::Value, Error> {
        let Kind::Number(n) = &self.kind else {
            return Err(self.mismatch(&v));
        };
        if n.is_float() {
            return Err(self.mismatch(&v));
        }
        macro_rules! fit {
            ($t:ty, $visit:ident, $name:literal) => {{
                let x = n
                    .as_i128()
                    .and_then(|x| <$t>::try_from(x).ok())
                    .or_else(|| n.as_u128().and_then(|x| <$t>::try_from(x).ok()));
                match x {
                    Some(x) => v.$visit(x),
                    None => Err(self.out_of_range($name)),
                }
            }};
        }
        match ty {
            IntTy::I8 => fit!(i8, visit_i8, "i8"),
            IntTy::I16 => fit!(i16, visit_i16, "i16"),
            IntTy::I32 => fit!(i32, visit_i32, "i32"),
            IntTy::I64 => fit!(i64, visit_i64, "i64"),
            IntTy::I128 => fit!(i128, visit_i128, "i128"),
            IntTy::U8 => fit!(u8, visit_u8, "u8"),
            IntTy::U16 => fit!(u16, visit_u16, "u16"),
            IntTy::U32 => fit!(u32, visit_u32, "u32"),
            IntTy::U64 => fit!(u64, visit_u64, "u64"),
            IntTy::U128 => fit!(u128, visit_u128, "u128"),
        }
    }
}

/// Integer target types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
}

/// A deserializer over one resolved scalar (mapping keys, `Some(x)`).
pub(crate) struct ScalarDe<'de>(pub(crate) Sc<'de>);

/// `!Name text` as an enum: variant `Name`, content the text as a string,
/// or (a typed enum under `Settings::resolve_tagged_variant_content`) the
/// text resolved with [`Sc::variant_content`] by a newtype variant.
struct TaggedScalar<'de> {
    sc: Sc<'de>,
    /// Read through `deserialize_enum`, not `deserialize_any`.
    typed_enum: bool,
    /// The enum's variant names (`deserialize_enum`).
    names: Option<&'static [&'static str]>,
}

/// The variant names a typed enum asked for (`deserialize_enum`), with
/// the memo of one read: which of them start with which
/// `%TAG` prefix.
#[derive(Clone, Copy)]
pub(crate) struct Variants<'m> {
    pub(crate) names: &'static [&'static str],
    pub(crate) memo: &'m RefCell<VariantMemo>,
}

/// One read's memo for matching `%TAG`-prefixed tags against an enum's
/// variant names: for each (prefix, variant list) pair
/// met, whether each variant name starts with the prefix's part of the
/// name, decided lazily, once per variant, and only for a variant whose
/// length and tail already match a tag. So a use compares only its suffix;
/// the prefix is compared once per read, against a variant name the
/// program wrote (never longer than that name). Keyed by addresses: the
/// prefixes are kept alive here, the variant lists are `'static`; not
/// keyed by document content, so not input-choosable (as
/// `IdHasher`); a `BTreeMap` keeps it `alloc`-only.
///
/// One read: a whole `from_str`/`from_value`/`Document::deserialize`, or
/// one `NodeRef::deserialize` call (which compares the prefix once per
/// call).
#[derive(Default)]
pub(crate) struct VariantMemo {
    starts: BTreeMap<MemoKey, Box<[Option<bool>]>>,
    keep: Vec<Arc<TagPrefix>>,
}

/// (Prefix address, variant list address, its length.)
type MemoKey = (usize, usize, usize);

/// A shared [`VariantMemo`] (a read's `Core` and the scalars it hands to
/// typed visitors), with the read's byte meter.
pub(crate) type SharedMemo = SharedRead;

/// The prefix's part of a variant name: its text without the tag's `!`
/// (`variant_name`'s rule). `None` when the prefix is empty (the `!`, if
/// any, is in the suffix).
fn name_head(p: &TagPrefix) -> Option<&str> {
    let t = p.text();
    if t.is_empty() {
        None
    } else {
        Some(t.strip_prefix('!').unwrap_or(t))
    }
}

impl VariantMemo {
    /// Whether variant `i` of `names` starts with `head` (the prefix's
    /// part of the name): O(`head`) the first time in this read, then O(1).
    fn starts(
        &mut self,
        p: &Arc<TagPrefix>,
        head: &str,
        names: &'static [&'static str],
        i: usize,
    ) -> bool {
        let key = (Arc::as_ptr(p).addr(), names.as_ptr().addr(), names.len());
        if !self.starts.contains_key(&key) {
            self.keep.push(Arc::clone(p));
        }
        let row = self
            .starts
            .entry(key)
            .or_insert_with(|| vec![None; names.len()].into_boxed_slice());
        match row.get_mut(i) {
            Some(Some(known)) => *known,
            Some(slot) => {
                let v = names.get(i).is_some_and(|n| n.starts_with(head));
                *slot = Some(v);
                v
            }
            None => false,
        }
    }
}

/// The variant `tag` names among `variants`, when it is one: the
/// `'static` name itself, found in O(suffix + the number of variants)
/// per use for a `%TAG`-prefixed tag, never joining its prefix.
fn matching_variant(tag: &ValueTag, variants: Variants<'_>) -> Option<&'static str> {
    let p = tag.shared_prefix()?;
    let head = name_head(p)?;
    let tail = tag.suffix();
    let len = head.len().checked_add(tail.len())?;
    let mut memo = variants.memo.try_borrow_mut().ok()?;
    for (i, n) in variants.names.iter().enumerate() {
        // Length, then the tail (O(suffix)), then the head (memoised).
        if n.len() == len
            && n.get(head.len()..) == Some(tail)
            && memo.starts(p, head, variants.names, i)
        {
            return Some(n);
        }
    }
    None
}

/// The variant name of a custom tag: a local tag without its `!`.
///
/// A typed enum (`variants`) gets its own `'static` name when the tag
/// matches one, without copying a `%TAG` prefix. Otherwise a
/// tag held in one piece (`!Name`, verbatim, default handles) costs its
/// own text; a `%TAG`-prefixed tag that names no variant (read through
/// `#[serde(other)]`, a hand-written visitor, or `deserialize_any`) is
/// joined by [`joined_variant_name`], in O(its length) per use, charged to
/// `meter` (the read's alias byte budget; `None`: a `from_value`
/// read, whose `Value`'s size was bounded when it was read, or is the
/// caller's own).
///
/// # Errors
///
/// `Y2009` at `span` when the joined name does not fit the byte budget.
pub(crate) fn variant_name(
    tag: &ValueTag,
    variants: Option<Variants<'_>>,
    meter: Option<&ByteMeter>,
    span: Span,
) -> Result<Cow<'static, str>, Error> {
    if let Some(n) = variants.and_then(|v| matching_variant(tag, v)) {
        return Ok(Cow::Borrowed(n));
    }
    let parts = tag.parts();
    let name = parts.strip_prefix("!").unwrap_or(parts);
    // A tag keeps the escapes of non-ASCII bytes and `%25` as written; a
    // variant is named by the characters they spell (the form a serializer
    // writes for a name with such characters).
    if name.bytes().any(|b| b == b'%') {
        let joined = joined_variant_name(name, meter, span)?;
        let decoded = percent_decoded(&joined).unwrap_or(joined);
        if let Some(n) = variants.and_then(|v| v.names.iter().find(|n| **n == decoded)) {
            return Ok(Cow::Borrowed(n));
        }
        return Ok(Cow::Owned(decoded));
    }
    Ok(match name.as_single() {
        Some(s) => Cow::Owned(s.to_owned()),
        None => Cow::Owned(joined_variant_name(name, meter, span)?),
    })
}

/// `s` with every `%XX` escape decoded, when the result is UTF-8 (`None`
/// otherwise: the name stays as written).
fn percent_decoded(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        let hex = b
            .get(i.saturating_add(1)..i.saturating_add(3))
            .and_then(|h| core::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (c, hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i = i.saturating_add(3);
            }
            _ => {
                out.push(c);
                i = i.saturating_add(1);
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The one place a `%TAG` tag's shared prefix is copied on a read: the
/// variant name of a tag that names no variant of the target enum, joined
/// in O(its length), its bytes charged to the alias byte
/// budget **before** they are copied.
fn joined_variant_name(
    name: TagStr<'_>,
    meter: Option<&ByteMeter>,
    span: Span,
) -> Result<String, Error> {
    if let Some(m) = meter {
        m.charge(name.len(), Some(span))?;
    }
    Ok(name.to_owned_string())
}

impl<'de> de::EnumAccess<'de> for TaggedScalar<'de> {
    type Error = Error;
    type Variant = ScalarContent<'de>;

    fn variant_seed<S: de::DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Error> {
        let Sc {
            kind,
            value,
            variant_content,
            span,
            depth,
            max,
            memo,
            ..
        } = self.sc;
        let fallback = RefCell::default();
        let variants = self.names.map(|names| Variants {
            names,
            memo: memo.as_deref().map_or(&fallback, |m| &m.variants),
        });
        let meter = memo.as_deref().map(|m| &m.meter);
        let (name, tag) = match kind {
            Kind::Custom(t) => (variant_name(t.tag(), variants, meter, span)?, Some(t)),
            _ => (Cow::Borrowed(""), None),
        };
        let variant = seed
            .deserialize(name.into_deserializer())
            .map_err(|e: Error| e.at(span))?;
        // The content of a custom-tagged scalar is a string (the resolver gives
        // a custom tag no licence to read `!x 12` as a number), except under
        // the opt-in `resolve_tagged_variant_content` in a typed enum (an
        // opt-in departure from the defaults): then a newtype variant reads it
        // as the plain scalar it was written as
        // (`ScalarContent::newtype_variant_seed`).
        let content = Sc {
            kind: Kind::Str,
            value,
            lenient: false,
            yaml11_hint: false,
            variant_content: variant_content.filter(|_| self.typed_enum),
            span,
            depth,
            max,
            memo,
        };
        Ok((variant, ScalarContent { sc: content, tag }))
    }
}

/// The content of a tagged scalar variant, and its tag.
pub(crate) struct ScalarContent<'de> {
    sc: Sc<'de>,
    tag: Option<TagText<'de>>,
}

impl<'de> de::VariantAccess<'de> for ScalarContent<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        if self.sc.value.is_empty() {
            Ok(())
        } else {
            Err(de::Error::invalid_type(
                Unexpected::Str(&self.sc.value),
                &"a unit variant (no content)",
            ))
        }
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        let mut sc = self.sc;
        let span = sc.span;
        if let Some(r) = sc.variant_content.take() {
            // Resolved here, only for a typed enum's newtype variant; a string
            // target still gets the text (`!Named 123` into a `String` is its
            // text).
            let resolved = r
                .variant_content(
                    self.tag.as_ref().map_or(TagStr::from(""), TagText::parts),
                    Style::Plain,
                    &sc.value,
                )
                .map_err(|e| resolve_error(e, &sc.value, span))?;
            if let Some(kind) = resolved.and_then(Kind::plain) {
                sc.kind = kind;
                sc.lenient = true;
            }
        }
        seed.deserialize(ScalarDe(sc.reenter()?))
            .map_err(|e| e.at(span))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, v: V) -> Result<V::Value, Error> {
        Err(de::Error::invalid_type(Unexpected::Str(&self.sc.value), &v))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        Err(de::Error::invalid_type(Unexpected::Str(&self.sc.value), &v))
    }
}

macro_rules! scalar_methods {
    ($($method:ident => $hint:expr),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
            let span = self.0.span;
            self.0.visit($hint, v).map_err(|e| e.at(span))
        }
    )*};
}

impl<'de> de::Deserializer<'de> for ScalarDe<'de> {
    type Error = Error;

    scalar_methods! {
        deserialize_any => Hint::Any,
        deserialize_bool => Hint::Bool,
        deserialize_i8 => Hint::Int(IntTy::I8),
        deserialize_i16 => Hint::Int(IntTy::I16),
        deserialize_i32 => Hint::Int(IntTy::I32),
        deserialize_i64 => Hint::Int(IntTy::I64),
        deserialize_i128 => Hint::Int(IntTy::I128),
        deserialize_u8 => Hint::Int(IntTy::U8),
        deserialize_u16 => Hint::Int(IntTy::U16),
        deserialize_u32 => Hint::Int(IntTy::U32),
        deserialize_u64 => Hint::Int(IntTy::U64),
        deserialize_u128 => Hint::Int(IntTy::U128),
        deserialize_f32 => Hint::Float,
        deserialize_f64 => Hint::Float,
        deserialize_char => Hint::Str,
        deserialize_str => Hint::Str,
        deserialize_string => Hint::Str,
        deserialize_bytes => Hint::Str,
        deserialize_byte_buf => Hint::Str,
        deserialize_option => Hint::Option,
        deserialize_unit => Hint::Unit,
        deserialize_seq => Hint::Seq,
        deserialize_map => Hint::Map,
        deserialize_identifier => Hint::Str,
        deserialize_ignored_any => Hint::Ignored,
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_unit(v)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        if name == crate::value::VALUE_MAGIC {
            return crate::value::hand_off(self.0.to_value(), v);
        }
        let span = self.0.span;
        self.0.visit(Hint::Newtype, v).map_err(|e| e.at(span))
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, v: V) -> Result<V::Value, Error> {
        self.deserialize_seq(v)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: usize,
        v: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(v)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        if crate::spanned::is_spanned(name, fields) {
            let span = self.0.span;
            let inner = ScalarDe(self.0.reenter()?);
            return v.visit_map(crate::spanned::SpannedAccess::new(
                span.start, span.end, inner,
            ));
        }
        self.deserialize_map(v)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        let span = self.0.span;
        self.0
            .visit(Hint::Enum(variants), v)
            .map_err(|e| e.at(span))
    }
}

/// An enum access whose variant is refused with an error.
struct RefusedVariant(Error);

/// The variant access of [`RefusedVariant`], never made.
enum NoVariant {}

impl<'de> de::EnumAccess<'de> for RefusedVariant {
    type Error = Error;
    type Variant = NoVariant;

    fn variant_seed<S: de::DeserializeSeed<'de>>(
        self,
        _: S,
    ) -> Result<(S::Value, NoVariant), Error> {
        Err(self.0)
    }
}

impl<'de> de::VariantAccess<'de> for NoVariant {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        match self {}
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, _: T) -> Result<T::Value, Error> {
        match self {}
    }

    fn tuple_variant<V: de::Visitor<'de>>(self, _: usize, _: V) -> Result<V::Value, Error> {
        match self {}
    }

    fn struct_variant<V: de::Visitor<'de>>(
        self,
        _: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Error> {
        match self {}
    }
}
