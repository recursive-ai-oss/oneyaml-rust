//! [`Tag`]: a resolved tag, held as a shared `%TAG` prefix plus its own
//! suffix.
//!
//! A shorthand tag under a `%TAG` directive is the directive's prefix
//! followed by the tag's suffix (YAML 1.2.2 §6.8.2.2). The prefix may be
//! megabytes and used by every node of a document, so a tag never copies
//! it: every tag of one read (or one `Document`) that uses a directive
//! shares one decoded copy of its prefix, and a tag costs only its suffix,
//! in time and memory.
//!
//! Identity is the **logical text** (prefix + suffix), whatever the split:
//! `%TAG !e! tag:x,1:` + `!e!a`, `!<tag:x,1:a>`, and `Tag::new("tag:x,1:a")`
//! are equal, hash equal, and order equal. Equality and hashing
//! cost O(1) for tags sharing one allocation (every use of one tag in a
//! read), O(suffix) for tags sharing a prefix, and O(length) otherwise;
//! the hash is a polynomial digest of the logical text (random base, per
//! process, modulo the Mersenne prime 2^61 − 1: hash flooding resistance)
//! computed once when the tag is built, from the prefix's own digest and
//! the suffix, in O(suffix).
//!
//! There is deliberately no `as_str()`: the text is not held in one piece.
//! Use [`Display`](fmt::Display) (`to_string()`), comparisons with `str`,
//! [`Tag::len`], or [`Tag::starts_with`].

use std::cmp::Ordering;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::{Arc, OnceLock};

/// 2^61 − 1.
const P: u64 = (1 << 61) - 1;

/// The digest's base: random per process, in `[2^20, P)`.
fn base() -> u64 {
    static BASE: OnceLock<u64> = OnceLock::new();
    *BASE.get_or_init(|| {
        let r = std::collections::hash_map::RandomState::new().hash_one(0x0000_e7a9_u64);
        r.wrapping_rem(P - (1 << 20)).wrapping_add(1 << 20)
    })
}

fn mulmod(a: u64, b: u64) -> u64 {
    let x = u128::from(a).wrapping_mul(u128::from(b));
    // x < 2^122: fold twice.
    let lo = (x & u128::from(P)) as u64;
    let hi = (x >> 61) as u64;
    let s = lo.wrapping_add(hi);
    let s = (s & P).wrapping_add(s >> 61);
    if s >= P { s.wrapping_sub(P) } else { s }
}

fn addmod(a: u64, b: u64) -> u64 {
    let s = a.wrapping_add(b);
    if s >= P { s.wrapping_sub(P) } else { s }
}

/// The digest of `text` and `base^len(text)`: O(text).
fn digest(text: &str) -> (u64, u64) {
    let b = base();
    let (mut h, mut pow) = (0u64, 1u64);
    for byte in text.bytes() {
        h = addmod(mulmod(h, b), u64::from(byte).wrapping_add(1));
        pow = mulmod(pow, b);
    }
    (h, pow)
}

/// A decoded `%TAG` prefix, shared by every tag of a read that uses it.
/// Its text is the prefix up to its last character boundary; the bytes of
/// a character the suffix completes (`%C3` + `%A9`) begin each tag's
/// suffix instead, so both parts are always `str`.
#[derive(Debug)]
pub(crate) struct TagPrefix {
    text: Box<str>,
    digest: u64,
}

impl TagPrefix {
    /// A prefix of `text` (whole characters). O(text), once per directive.
    pub(crate) fn new(text: Box<str>) -> Self {
        let (digest, _) = digest(&text);
        Self { text, digest }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Its (logical-text) digest, for interning by content.
    pub(crate) fn digest(&self) -> u64 {
        self.digest
    }
}

/// A resolved tag, e.g. `!color` or `tag:example.com,2000:app/foo`, after
/// `%TAG` expansion and `%XX` decoding.
///
/// A tag read under a `%TAG` directive holds the directive's prefix,
/// shared with every tag of the same read or `Document` that uses it, and
/// its own suffix: it never copies the prefix, which may be long.
/// Identity is the whole text, however it is split or was spelled:
/// `%TAG !e! tag:x,1:` + `!e!a`, `!<tag:x,1:a>` and `Tag::new("tag:x,1:a")`
/// are equal, hash equal and order equal.
///
/// There is no `as_str()`, as in `serde_yaml`: the text is not held in one
/// piece. Use `Display` (`to_string()`), comparisons with `str`
/// (`tag == "!x"`), [`Tag::len`], [`Tag::starts_with`], [`Tag::ends_with`].
///
/// Cloning is O(1); hashing is O(1); comparing two tags of one read is
/// O(1), otherwise O(the text); comparing with a `str` is O(that `str`).
///
/// ```
/// use oneyaml::{Tag, Value, from_str};
///
/// let v: Value = from_str("%TAG !e! tag:example.com,2000:\n--- !e!point [1, 2]\n").unwrap();
/// let tag = &v.as_tagged().unwrap().tag;
/// assert_eq!(*tag, Tag::new("tag:example.com,2000:point"));
/// assert!(*tag == "tag:example.com,2000:point");
/// assert_eq!(tag.to_string(), "tag:example.com,2000:point");
/// assert!(tag.starts_with("tag:example.com") && tag.ends_with(":point"));
/// ```
#[derive(Clone)]
pub struct Tag {
    prefix: Option<Arc<TagPrefix>>,
    suffix: Arc<str>,
    digest: u64,
}

/// A tag's text as up to two pieces, without copying (crate-internal).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TagStr<'a> {
    a: &'a str,
    b: &'a str,
}

impl<'a> From<&'a str> for TagStr<'a> {
    fn from(s: &'a str) -> Self {
        Self { a: s, b: "" }
    }
}

impl<'a> TagStr<'a> {
    pub(crate) fn len(self) -> usize {
        self.a.len().saturating_add(self.b.len())
    }

    /// The text in one piece, when it is one (no copy).
    pub(crate) fn as_single(self) -> Option<&'a str> {
        if self.b.is_empty() {
            Some(self.a)
        } else if self.a.is_empty() {
            Some(self.b)
        } else {
            None
        }
    }

    pub(crate) fn starts_with(self, p: &str) -> bool {
        self.strip_prefix(p).is_some()
    }

    /// The rest after `p`, when the text starts with it. O(p).
    pub(crate) fn strip_prefix(self, p: &str) -> Option<Self> {
        if let Some(rest) = self.a.strip_prefix(p) {
            return Some(Self { a: rest, b: self.b });
        }
        // `p` runs past `a` into `b`.
        let (head, tail) = (p.get(..self.a.len())?, p.get(self.a.len()..)?);
        if head != self.a {
            return None;
        }
        let rest = self.b.strip_prefix(tail)?;
        Some(Self { a: rest, b: "" })
    }

    pub(crate) fn ends_with(self, p: &str) -> bool {
        match p.len().checked_sub(self.b.len()) {
            None => self.b.ends_with(p),
            Some(cut) => {
                p.get(cut..).is_some_and(|tail| tail == self.b)
                    && p.get(..cut).is_some_and(|head| self.a.ends_with(head))
            }
        }
    }

    pub(crate) fn bytes(self) -> impl Iterator<Item = u8> + 'a {
        self.a.bytes().chain(self.b.bytes())
    }

    pub(crate) fn chars(self) -> core::iter::Chain<core::str::Chars<'a>, core::str::Chars<'a>> {
        self.a.chars().chain(self.b.chars())
    }

    /// Whether the text is `s`. O(s).
    pub(crate) fn is(self, s: &str) -> bool {
        self.len() == s.len() && self.strip_prefix(s).is_some()
    }

    /// The text, copied (O(length)): only where one `&str` is required.
    pub(crate) fn to_owned_string(self) -> String {
        let mut out = String::with_capacity(self.len());
        out.push_str(self.a);
        out.push_str(self.b);
        out
    }
}

impl PartialEq for TagStr<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.bytes().eq(other.bytes())
    }
}

impl Eq for TagStr<'_> {}

impl PartialEq<&str> for TagStr<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.is(other)
    }
}

impl fmt::Display for TagStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.a)?;
        f.write_str(self.b)
    }
}

impl Tag {
    /// A tag from its resolved text.
    #[must_use]
    pub fn new(tag: impl Into<String>) -> Self {
        Self::from_parts(None, Arc::from(tag.into()))
    }

    /// The tag `prefix` + `suffix`: O(suffix), the prefix shared.
    pub(crate) fn from_parts(prefix: Option<Arc<TagPrefix>>, suffix: Arc<str>) -> Self {
        let (hs, pow) = digest(&suffix);
        let digest = match &prefix {
            Some(p) => addmod(mulmod(p.digest, pow), hs),
            None => hs,
        };
        Self {
            prefix,
            suffix,
            digest,
        }
    }

    /// The text's length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parts().len()
    }

    /// Whether the text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the text starts with `prefix`. O(`prefix`).
    #[must_use]
    pub fn starts_with(&self, prefix: &str) -> bool {
        self.parts().starts_with(prefix)
    }

    /// Whether the text ends with `suffix`. O(`suffix`).
    #[must_use]
    pub fn ends_with(&self, suffix: &str) -> bool {
        self.parts().ends_with(suffix)
    }

    /// Whether `self` and `other` are one allocation (so comparing them is
    /// O(1)): every use of one tag in one read or `Document` is. For tests
    /// of that sharing; only with the unstable
    /// `__internal-checks` feature, not part of the stability
    /// promise.
    #[cfg(feature = "__internal-checks")]
    #[doc(hidden)]
    #[must_use]
    pub fn same_allocation(&self, other: &Tag) -> bool {
        self.same(other)
    }

    /// Whether `self` holds the `%TAG` prefix `other` holds (one shared
    /// copy). For tests; only with the unstable
    /// `__internal-checks` feature, not part of the stability
    /// promise.
    #[cfg(feature = "__internal-checks")]
    #[doc(hidden)]
    #[must_use]
    pub fn shares_prefix_with(&self, other: &Tag) -> bool {
        self.prefix.is_some() && self.same_prefix(other)
    }

    /// The text as its two pieces (crate-internal).
    pub(crate) fn parts(&self) -> TagStr<'_> {
        TagStr {
            a: self.prefix.as_ref().map_or("", |p| p.text()),
            b: &self.suffix,
        }
    }

    /// Its own part of the text (after the shared prefix).
    pub(crate) fn suffix(&self) -> &str {
        &self.suffix
    }

    /// The prefix this tag shares, if any.
    pub(crate) fn shared_prefix(&self) -> Option<&Arc<TagPrefix>> {
        self.prefix.as_ref()
    }

    /// Its digest (equal texts, equal digests).
    pub(crate) fn digest(&self) -> u64 {
        self.digest
    }

    /// The allocation's identity: equal for two tags exactly when
    /// [`Tag::same`] (while both are alive).
    pub(crate) fn identity(&self) -> (usize, usize) {
        (
            Arc::as_ptr(&self.suffix).cast::<u8>().addr(),
            self.prefix.as_ref().map_or(0, |p| Arc::as_ptr(p).addr()),
        )
    }

    /// Same allocation: equal in O(1).
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.suffix, &other.suffix) && self.same_prefix(other)
    }

    pub(crate) fn same_prefix(&self, other: &Self) -> bool {
        match (&self.prefix, &other.prefix) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl PartialEq for Tag {
    fn eq(&self, other: &Self) -> bool {
        if self.same(other) {
            return true;
        }
        if self.digest != other.digest || self.len() != other.len() {
            return false;
        }
        if self.same_prefix(other) {
            return self.suffix == other.suffix;
        }
        self.parts() == other.parts()
    }
}

impl Eq for Tag {}

impl Hash for Tag {
    /// The logical text's digest: O(1) (computed when the tag was built).
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.digest);
    }
}

impl PartialOrd for Tag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Tag {
    /// The logical text's byte order. O(common length) (O(1) for the same
    /// allocation).
    fn cmp(&self, other: &Self) -> Ordering {
        if self.same(other) {
            return Ordering::Equal;
        }
        if self.same_prefix(other) {
            return self.suffix.cmp(&other.suffix);
        }
        self.parts().bytes().cmp(other.parts().bytes())
    }
}

impl PartialEq<str> for Tag {
    fn eq(&self, other: &str) -> bool {
        self.parts().is(other)
    }
}

impl PartialEq<&str> for Tag {
    fn eq(&self, other: &&str) -> bool {
        self.parts().is(other)
    }
}

impl PartialEq<String> for Tag {
    fn eq(&self, other: &String) -> bool {
        self.parts().is(other)
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.parts(), f)
    }
}

impl fmt::Debug for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tag({self})")
    }
}

/// The tag `!name` of an enum variant `name`, in the form a document gives
/// it: a `%` is written `%25` and a non-ASCII character as the escapes of its
/// UTF-8 bytes, which a tag keeps as written (the variant is matched by
/// the name they spell). Every other character is left for the writer.
pub(crate) fn variant_tag_text(name: &str) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(name.len().saturating_add(1));
    out.push('!');
    for c in name.chars() {
        if c == '%' || !c.is_ascii() {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                let _ = write!(out, "%{b:02X}");
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use std::collections::hash_map::RandomState;

    fn split(p: &str, s: &str) -> Tag {
        Tag::from_parts(Some(Arc::new(TagPrefix::new(p.into()))), Arc::from(s))
    }

    #[test]
    fn identity_is_the_logical_text() {
        let s = RandomState::new();
        let whole = Tag::new("tag:x,1:abc");
        for t in [
            split("tag:x,1:", "abc"),
            split("tag:x,1:ab", "c"),
            split("", "tag:x,1:abc"),
            split("tag:x,1:abc", ""),
        ] {
            assert_eq!(t, whole);
            assert_eq!(s.hash_one(&t), s.hash_one(&whole));
            assert_eq!(t.cmp(&whole), Ordering::Equal);
            assert_eq!(t.to_string(), "tag:x,1:abc");
            assert_eq!(t.len(), 11);
            assert!(t == "tag:x,1:abc" && t != "tag:x,1:ab" && t != "tag:x,1:abcd");
        }
        assert_ne!(split("a", "b"), Tag::new("ac"));
        assert_ne!(split("a", "b"), Tag::new("ab "));
        assert!(split("a", "b") < Tag::new("ac"));
        assert!(split("ab", "") > Tag::new("a"));
        assert!(split("tag:x,", "1").starts_with("tag:x,1"));
        assert!(!split("tag:x,", "1").starts_with("tag:x,12"));
        assert!(split("tag:x,", "1").ends_with("x,1"));
        assert!(split("tag:x,", "1").ends_with("1"));
        assert!(split("tag:x,", "1").ends_with("tag:x,1"));
        assert!(!split("tag:x,", "1").ends_with("!tag:x,1"));
        assert!(!split("tag:x,", "1").ends_with("y,1"));
        assert_eq!(format!("{:?}", split("!", "a")), "Tag(!a)");
    }

    #[test]
    fn tag_str_pieces() {
        let t = split("ab", "cd");
        let p = t.parts();
        assert_eq!(p.strip_prefix("a").unwrap().to_string(), "bcd");
        assert_eq!(p.strip_prefix("abc").unwrap().to_string(), "d");
        assert!(p.strip_prefix("abd").is_none());
        assert!(p.strip_prefix("abcde").is_none());
        assert_eq!(p.strip_prefix("abcd").unwrap().len(), 0);
        assert_eq!(p.as_single(), None);
        assert_eq!(p.strip_prefix("abc").unwrap().as_single(), Some("d"));
        assert_eq!(p.chars().collect::<String>(), "abcd");
    }

    #[test]
    fn digests_combine() {
        let (hp, _) = digest("prefix-");
        let (hs, pow) = digest("suffix");
        let (hw, _) = digest("prefix-suffix");
        assert_eq!(addmod(mulmod(hp, pow), hs), hw);
        assert!(mulmod(P - 1, P - 1) < P);
    }
}
