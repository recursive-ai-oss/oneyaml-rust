//! Reading meaning from the source on demand (event representation,
//! kept small): the accessors of [`Event`](crate::event::Event) call these, and
//! each reuses the scanner the parser used to find the same bytes (one
//! reading of the input).

use std::borrow::Cow;

use oneyaml_core::Span;

use super::block_scalar::block_value;
use super::scalar::{plain_value, quoted};
use super::scan::{anchor_end, byte, decoded, percent_decode_concat, scan_tag, tag_prefix_at};
use super::stream::parse_directive;
use crate::event::{Anchor, BlockHeader, DirectiveKind, ScalarStyle, Tag, TagForm, TagSource};

/// The anchor property whose `&` is at `start`.
pub(crate) fn anchor_at(src: &str, start: usize) -> Option<Anchor<'_>> {
    if byte(src, start) != Some(b'&') {
        return None;
    }
    let from = start.checked_add(1)?;
    let end = anchor_end(src, from);
    let name = src.get(from..end).filter(|n| !n.is_empty())?;
    Some(Anchor::new(name, Span::new(start, end)))
}

/// The tag property whose `!` is at `start`, as written: O(the
/// property's text), never reading the `%TAG` prefix (which starts at
/// `prefix`, when its handle names one).
pub(crate) fn tag_source_at(
    src: &str,
    start: usize,
    prefix: Option<usize>,
) -> Option<TagSource<'_>> {
    let t = scan_tag(src, start)?;
    let handle = src.get(t.handle.0..t.handle.1)?;
    let suffix = src.get(t.suffix.0..t.suffix.1)?;
    let span = Span::new(start, t.end);
    Some(TagSource::new(t.form, handle, suffix, span, src, prefix))
}

/// [`TagSource::resolve`] (§6.9.1, §6.8.2): the tag against its `%TAG`
/// prefix, else the default handles `!` and `!!`. `%XX` escapes of the
/// whole URI (prefix and suffix) are decoded; the result borrows
/// the source when it is a slice of it.
pub(crate) fn resolve_tag<'a>(t: &TagSource<'a>) -> Option<Tag<'a>> {
    let src = t.src();
    let (handle, suffix, span) = (t.handle, t.suffix, t.span);
    let at = t.at();
    let text = src.get(at.start..at.end)?;
    let resolved = match t.form {
        TagForm::NonSpecific => Cow::Borrowed("!"),
        TagForm::Verbatim if suffix.contains('%') => Cow::Owned(percent_decode_concat("", suffix)),
        TagForm::Verbatim => Cow::Borrowed(suffix),
        _ => {
            let prefix = match (t.prefix(), handle) {
                // A `%TAG` prefix is never empty (production 93).
                (Some(p), _) => tag_prefix_at(src, p)?,
                (None, "!") => "!",
                (None, "!!") => "tag:yaml.org,2002:",
                (None, _) => return None,
            };
            // `!suffix` under the prefix `!` resolves to its own text.
            if prefix == "!" && handle == "!" && !suffix.contains('%') {
                Cow::Borrowed(text)
            } else {
                // §6.8.2.2: prefix + suffix is one URI, decoded
                // as one string, so `%TAG !e! a:%21` + `!e!x`, `!<a:%21x>` and
                // `%TAG !e! a:` + `!e!%21x` all resolve to `a:!x`.
                Cow::Owned(percent_decode_concat(prefix, suffix))
            }
        }
    };
    let (handle, suffix) = match t.form {
        TagForm::Verbatim => ("", suffix),
        TagForm::NonSpecific => ("!", ""),
        _ => (handle, suffix),
    };
    Some(Tag::new(t.form, handle, suffix, resolved, span))
}

/// [`TagSource::directive_prefix`]: the decoded `%TAG` prefix, borrowed
/// when it has no escapes.
pub(crate) fn directive_prefix<'a>(t: &TagSource<'a>) -> Option<Cow<'a, [u8]>> {
    let p = tag_prefix_at(t.src(), t.prefix()?)?;
    Some(if p.contains('%') {
        Cow::Owned(decoded(p).collect())
    } else {
        Cow::Borrowed(p.as_bytes())
    })
}

/// [`TagSource::decoded_suffix`].
pub(crate) fn decoded_suffix<'a>(t: &TagSource<'a>) -> Cow<'a, [u8]> {
    if t.suffix.contains('%') {
        Cow::Owned(decoded(t.suffix).collect())
    } else {
        Cow::Borrowed(t.suffix.as_bytes())
    }
}

/// The value of the scalar covering `span` (§7.3, §8.1): plain text folded,
/// quoted text unescaped and folded, block text chomped and folded. `None`
/// when the bytes are not one complete scalar of that style.
pub(crate) fn scalar_value(
    src: &str,
    span: Span,
    style: ScalarStyle,
    block: Option<BlockHeader>,
) -> Option<Cow<'_, str>> {
    let text = src.get(span.start..span.end)?;
    match style {
        ScalarStyle::Plain if super::run::find_break(text.as_bytes(), 0) < text.len() => {
            Some(Cow::Owned(plain_value::<String>(text)))
        }
        ScalarStyle::Plain => Some(Cow::Borrowed(text)),
        ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted => {
            let double = style == ScalarStyle::DoubleQuoted;
            // The parser validated the text against its parent's
            // indentation; the value does not depend on it.
            // The closing quote must end the span.
            let f = quoted::<String>(text, 0, None, double)
                .ok()
                .filter(|f| f.end == text.len())?;
            match f.value {
                Some(owned) => Some(Cow::Owned(owned)),
                None => text.get(1..f.end.saturating_sub(1)).map(Cow::Borrowed),
            }
        }
        ScalarStyle::Literal | ScalarStyle::Folded => {
            let folded = style == ScalarStyle::Folded;
            block_value(src, span, block?, folded).map(Cow::Owned)
        }
    }
}

/// The directive covering `span`.
pub(crate) fn directive_at(src: &str, span: Span) -> Option<DirectiveKind<'_>> {
    parse_directive(src, span.start)
        .filter(|d| d.end == span.end)
        .map(|d| d.kind)
}
