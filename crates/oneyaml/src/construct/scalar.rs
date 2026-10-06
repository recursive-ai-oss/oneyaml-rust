//! Resolved scalars as values, and resolver errors as diagnostics.

use std::borrow::Cow;

use oneyaml_core::{Span, code};

use super::Tag;
use crate::error::Error;
use crate::resolve::{
    CollectionKind, CoreTag, ResolveError, Resolved, ResolvedCollection, Resolver, Style,
};
use crate::value::{TaggedValue, Value};

fn tag_name(t: CoreTag) -> &'static str {
    match t {
        CoreTag::Null => "!!null",
        CoreTag::Bool => "!!bool",
        CoreTag::Int => "!!int",
        CoreTag::Float => "!!float",
        CoreTag::Str => "!!str",
        CoreTag::Seq => "!!seq",
        CoreTag::Map => "!!map",
    }
}

fn shorten(text: &str) -> String {
    const MAX: usize = 40;
    match text.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", text.get(..cut).unwrap_or(text)),
        None => text.to_owned(),
    }
}

/// A resolver error as a located [`Error`].
pub(crate) fn resolve_error(e: ResolveError, text: &str, span: Span) -> Error {
    match e {
        ResolveError::InvalidForTag(t) => Error::new(
            code::INVALID_SCALAR_FOR_TAG,
            format!("`{}` is not a valid {}", shorten(text), tag_name(t)),
            Some(span),
        )
        .with_help(format!("fix the value, or remove the {} tag", tag_name(t))),
        ResolveError::CollectionTagOnScalar(t) => Error::new(
            code::COLLECTION_TAG_ON_SCALAR,
            format!("{} on a scalar", tag_name(t)),
            Some(span),
        ),
        ResolveError::WrongTagOnCollection(t) => Error::new(
            code::WRONG_TAG_ON_COLLECTION,
            format!("{} on a {text}", tag_name(t)),
            Some(span),
        ),
        ResolveError::JsonUnmatched => Error::new(
            code::JSON_UNMATCHED_SCALAR,
            format!(
                "plain scalar `{}` matches no JSON schema type",
                shorten(text)
            ),
            Some(span),
        )
        .with_help("quote it to make it a string, or use the Core schema"),
        _ => Error::internal("resolver invariant", Some(span)),
    }
}

/// Resolves a scalar event (the one resolver).
pub(crate) fn resolve<'t>(
    r: &Resolver,
    tag: &'t Tag<'_>,
    plain: bool,
    value: &str,
    span: Span,
) -> Result<Resolved<'t>, Error> {
    let style = if plain { Style::Plain } else { Style::NonPlain };
    r.resolve_scalar(tag.input(), style, value)
        .map_err(|e| resolve_error(e, value, span))
}

/// Resolves a collection's tag.
pub(crate) fn resolve_collection<'t>(
    r: &Resolver,
    tag: &'t Tag<'_>,
    kind: CollectionKind,
    span: Span,
) -> Result<ResolvedCollection<'t>, Error> {
    r.resolve_collection(tag.input(), kind).map_err(|e| {
        let what = match kind {
            CollectionKind::Sequence => "sequence",
            CollectionKind::Mapping => "mapping",
        };
        resolve_error(e, what, span)
    })
}

/// Whether a scalar event is a merge key (`<<` with `merge_keys` on).
pub(crate) fn is_merge_key(r: &Resolver, tag: &Tag<'_>, plain: bool, value: &str) -> bool {
    let style = if plain { Style::Plain } else { Style::NonPlain };
    r.is_merge_key(tag.input(), style, value)
}

/// The value of a resolved scalar with node tag `tag` (a custom tag's text
/// is shared with the other nodes of the read).
pub(crate) fn scalar_value(resolved: Resolved<'_>, text: Cow<'_, str>, tag: &Tag<'_>) -> Value {
    match resolved {
        Resolved::Null => Value::Null,
        Resolved::Bool(b) => Value::Bool(b),
        Resolved::Number(n) => Value::Number(n),
        Resolved::Str => Value::String(text.into_owned()),
        Resolved::Custom(t) => Value::Tagged(Box::new(TaggedValue {
            tag: tag.value_tag(t),
            value: Value::String(text.into_owned()),
        })),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn every_resolve_error_has_its_code() {
        let s = Span::new(0, 3);
        let cases = [
            (
                ResolveError::InvalidForTag(CoreTag::Int),
                code::INVALID_SCALAR_FOR_TAG,
            ),
            (
                ResolveError::CollectionTagOnScalar(CoreTag::Seq),
                code::COLLECTION_TAG_ON_SCALAR,
            ),
            (
                ResolveError::WrongTagOnCollection(CoreTag::Str),
                code::WRONG_TAG_ON_COLLECTION,
            ),
            (ResolveError::JsonUnmatched, code::JSON_UNMATCHED_SCALAR),
            (ResolveError::Internal, code::INTERNAL_ERROR),
        ];
        for (e, c) in cases {
            let err = resolve_error(e, "twelve", s);
            assert_eq!(err.code(), c);
            assert_eq!(err.span(), Some(s));
        }
        let long = "x".repeat(100);
        let e = resolve_error(ResolveError::JsonUnmatched, &long, s);
        assert!(e.diagnostic().message.contains('…'));
    }
}
