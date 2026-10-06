//! Paths to nodes, e.g. `spec.ports[2].port`.

/// One step of a [`Path`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PathSegment {
    /// A mapping key, as text.
    Key(String),
    /// A sequence index.
    Index(usize),
}

/// A path from the document root to a node.
///
/// A flat list of segments: no recursion anywhere in its impls.
///
/// ```
/// use oneyaml_core::{Path, PathSegment};
///
/// let p = Path::new().with_key("spec").with_key("ports").with_index(2);
/// assert_eq!(p.to_string(), "spec.ports[2]");
/// assert_eq!(p.segments().last(), Some(&PathSegment::Index(2)));
/// let mut q = p.clone();
/// q.push_key("port");
/// assert_eq!((q.len(), q.pop()), (4, Some(PathSegment::Key("port".into()))));
/// q.push_index(0);
/// assert_eq!(q.to_string(), "spec.ports[2][0]");
/// assert!(!q.is_empty() && Path::new().is_empty());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Path {
    segments: Vec<PathSegment>,
}

impl Path {
    /// The root path (no segments).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Appends a mapping-key segment.
    pub fn push_key(&mut self, key: impl Into<String>) {
        self.segments.push(PathSegment::Key(key.into()));
    }

    /// Appends a sequence-index segment.
    pub fn push_index(&mut self, index: usize) {
        self.segments.push(PathSegment::Index(index));
    }

    /// Chainable [`Path::push_key`].
    #[must_use]
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.push_key(key);
        self
    }

    /// Chainable [`Path::push_index`].
    #[must_use]
    pub fn with_index(mut self, index: usize) -> Self {
        self.push_index(index);
        self
    }

    /// Removes and returns the last segment.
    pub fn pop(&mut self) -> Option<PathSegment> {
        self.segments.pop()
    }

    /// The segments, root first.
    #[must_use]
    pub fn segments(&self) -> &[PathSegment] {
        &self.segments
    }

    /// `true` for the root path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Number of segments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.segments.len()
    }
}

/// Builds a [`Path`] from keys and indices:
/// `path!["spec", "containers", 0, "image"]`.
///
/// String-like segments (`&str`, `String`) are keys; integers are indices.
/// A negative index names no item (it becomes `usize::MAX`).
///
/// ```
/// use oneyaml_core::{path, Path};
/// let p = path!["spec", "ports", 2, "port"];
/// assert_eq!(p.to_string(), "spec.ports[2].port");
/// assert_eq!(path![], Path::new());
/// ```
#[macro_export]
macro_rules! path {
    () => { $crate::Path::new() };
    ($($segment:expr),+ $(,)?) => {{
        let mut p = $crate::Path::new();
        $( $crate::__private::push_segment(&mut p, $segment); )+
        p
    }};
}

/// Support for [`path!`]; not public API.
#[doc(hidden)]
pub mod __private {
    use super::{Path, PathSegment};

    /// A value `path!` accepts as a segment.
    pub trait IntoSegment {
        /// The segment.
        fn into_segment(self) -> PathSegment;
    }

    impl IntoSegment for &str {
        fn into_segment(self) -> PathSegment {
            PathSegment::Key(self.to_owned())
        }
    }
    impl IntoSegment for String {
        fn into_segment(self) -> PathSegment {
            PathSegment::Key(self)
        }
    }
    impl IntoSegment for &String {
        fn into_segment(self) -> PathSegment {
            PathSegment::Key(self.clone())
        }
    }
    impl IntoSegment for usize {
        fn into_segment(self) -> PathSegment {
            PathSegment::Index(self)
        }
    }
    // Unsuffixed integer literals are `i32`.
    impl IntoSegment for i32 {
        fn into_segment(self) -> PathSegment {
            PathSegment::Index(usize::try_from(self).unwrap_or(usize::MAX))
        }
    }
    impl IntoSegment for u32 {
        fn into_segment(self) -> PathSegment {
            PathSegment::Index(usize::try_from(self).unwrap_or(usize::MAX))
        }
    }
    impl IntoSegment for u64 {
        fn into_segment(self) -> PathSegment {
            PathSegment::Index(usize::try_from(self).unwrap_or(usize::MAX))
        }
    }

    /// Appends one segment.
    pub fn push_segment(path: &mut Path, segment: impl IntoSegment) {
        path.segments.push(segment.into_segment());
    }
}

/// Why a path string is not a [`Path`] (see [`Path::parse`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParsePathError {
    offset: usize,
    reason: &'static str,
}

impl ParsePathError {
    /// The UTF-8 byte offset in the path string where it went wrong.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// What went wrong.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl core::fmt::Display for ParsePathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid path at byte {}: {}", self.offset, self.reason)
    }
}

impl std::error::Error for ParsePathError {}

/// A character a bare (unquoted) key may hold: what [`Path`]'s `Display`
/// writes unquoted.
fn bare_char(c: char) -> bool {
    !(matches!(c, '.' | '[' | ']' | '"' | '\\') || c.is_whitespace() || c.is_control())
}

impl Path {
    /// Parses a path string: the inverse of `Display`, so
    /// `Path::parse(&p.to_string()) == Ok(p)` for every path.
    ///
    /// The syntax:
    ///
    /// * keys are separated by `.`: `spec.template.metadata`;
    /// * `[n]` (decimal digits) is a sequence index: `containers[0]`,
    ///   `[0]` at the start, `m[1][2]` for nested sequences;
    /// * a key holding `.`, `[`, `]`, `"`, `\`, white space or a control
    ///   character, or an empty key, is written in double quotes, where
    ///   `\"` is a quote, `\\` a backslash and `\u{hex}` any character:
    ///   `a."b.c"[0]`, `""` (the empty key), `"say \"hi\""`;
    /// * `.` alone is the root path (what `Display` writes for it); the
    ///   empty string is an error, so an empty key from user input never
    ///   names the whole document.
    ///
    /// Every key is text: `[n]` is always an index, and `a.1` is the key
    /// `"1"`.
    ///
    /// ```
    /// use oneyaml_core::{Path, path};
    ///
    /// let p = Path::parse("spec.containers[0].\"app.kubernetes.io/name\"").unwrap();
    /// assert_eq!(p, path!["spec", "containers", 0, "app.kubernetes.io/name"]);
    /// assert_eq!(Path::parse(&p.to_string()), Ok(p));
    /// assert_eq!(Path::parse("."), Ok(Path::new()));
    /// assert_eq!(
    ///     Path::parse("").unwrap_err().reason(),
    ///     "an empty path: write `.` for the root"
    /// );
    /// let err = Path::parse("a..b").unwrap_err();
    /// assert_eq!(err.offset(), 2);
    /// assert_eq!(err.reason(), "expected a key after `.`");
    /// ```
    ///
    /// # Errors
    ///
    /// [`ParsePathError`], with the byte offset and the reason, for
    /// anything else: the empty string, an empty segment (`a..b`, `a.`, `.a`), a bare key
    /// with a reserved character (`a b`, `a]`), an unclosed quote or `[`,
    /// a non-digit or overflowing index, an unknown escape, or text right
    /// after a quoted key or an index (`"a"b`, `[0]x`).
    pub fn parse(s: &str) -> Result<Path, ParsePathError> {
        let err = |offset: usize, reason: &'static str| ParsePathError { offset, reason };
        let mut path = Path::new();
        // `""` is not the root, so an empty user key handed to
        // `set_path` cannot replace the whole document; `Display` writes the
        // root as `.`, so the round trip is unaffected.
        if s.is_empty() {
            return Err(err(0, "an empty path: write `.` for the root"));
        }
        if s == "." {
            return Ok(path);
        }
        let mut chars = s.char_indices().peekable();
        // One loop over the characters, no recursion.
        let mut first = true;
        while let Some(&(at, c)) = chars.peek() {
            match c {
                '[' => {
                    chars.next();
                    let mut index: usize = 0;
                    let mut digits = 0usize;
                    loop {
                        match chars.next() {
                            Some((_, ']')) if digits > 0 => break,
                            Some((_, ']')) => return Err(err(at, "an index needs digits: `[0]`")),
                            Some((p, d)) => {
                                let Some(v) = d.to_digit(10) else {
                                    return Err(err(p, "an index is decimal digits only"));
                                };
                                index = index
                                    .checked_mul(10)
                                    .and_then(|i| i.checked_add(v as usize))
                                    .ok_or_else(|| err(at, "the index is too large"))?;
                                digits = digits.saturating_add(1);
                            }
                            None => return Err(err(at, "unclosed `[`")),
                        }
                    }
                    path.push_index(index);
                }
                '.' if !first => {
                    chars.next();
                    let next = chars.peek().copied();
                    match next {
                        Some((_, '"')) => path.push_key(quoted(&mut chars)?),
                        Some((p, c)) if bare_char(c) => {
                            path.push_key(bare(s, p, &mut chars));
                        }
                        Some((p, _)) => {
                            return Err(err(p, "expected a key after `.`"));
                        }
                        None => return Err(err(s.len(), "expected a key after `.`")),
                    }
                }
                '"' if first => path.push_key(quoted(&mut chars)?),
                c if first && bare_char(c) => path.push_key(bare(s, at, &mut chars)),
                '.' => return Err(err(at, "a path starts with a key or an index")),
                _ if first => {
                    return Err(err(at, "quote this key: `\"…\"`"));
                }
                _ => return Err(err(at, "expected `.` or `[` after a segment")),
            }
            first = false;
        }
        Ok(path)
    }
}

impl core::str::FromStr for Path {
    type Err = ParsePathError;

    /// As [`Path::parse`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Path::parse(s)
    }
}

type Chars<'a> = core::iter::Peekable<core::str::CharIndices<'a>>;

/// A bare key starting at `start` (its first character is a bare one).
fn bare(s: &str, start: usize, chars: &mut Chars<'_>) -> String {
    let mut end = s.len();
    while let Some(&(p, c)) = chars.peek() {
        if !bare_char(c) {
            end = p;
            break;
        }
        chars.next();
    }
    s.get(start..end).unwrap_or_default().to_owned()
}

/// A quoted key; the next character is its opening `"`.
fn quoted(chars: &mut Chars<'_>) -> Result<String, ParsePathError> {
    let err = |offset: usize, reason: &'static str| ParsePathError { offset, reason };
    let open = chars.next().map_or(0, |(p, _)| p);
    let mut key = String::new();
    loop {
        match chars.next() {
            Some((_, '"')) => return Ok(key),
            Some((p, '\\')) => match chars.next() {
                Some((_, '"')) => key.push('"'),
                Some((_, '\\')) => key.push('\\'),
                Some((_, 'u')) => {
                    if !matches!(chars.next(), Some((_, '{'))) {
                        return Err(err(p, "write `\\u{hex}`"));
                    }
                    let mut code: u32 = 0;
                    let mut digits = 0usize;
                    loop {
                        match chars.next() {
                            Some((_, '}')) if digits > 0 => break,
                            Some((_, h)) if digits < 6 && h.is_ascii_hexdigit() => {
                                let v = h.to_digit(16).unwrap_or(0);
                                code = code.wrapping_mul(16).wrapping_add(v);
                                digits = digits.saturating_add(1);
                            }
                            _ => return Err(err(p, "write `\\u{hex}` with 1 to 6 hex digits")),
                        }
                    }
                    let c = char::from_u32(code)
                        .ok_or_else(|| err(p, "`\\u{…}` names no character"))?;
                    key.push(c);
                }
                _ => return Err(err(p, "unknown escape: use `\\\"`, `\\\\` or `\\u{hex}`")),
            },
            Some((_, c)) => key.push(c),
            None => return Err(err(open, "unclosed `\"`")),
        }
    }
}

fn needs_quotes(key: &str) -> bool {
    key.is_empty()
        || key.chars().any(|c| {
            matches!(c, '.' | '[' | ']' | '"' | '\\') || c.is_whitespace() || c.is_control()
        })
}

impl core::fmt::Display for Path {
    /// `a.b[2].c`; keys that are empty or contain `.[]"\`, whitespace, or
    /// control characters are written as `"…"` with `\"`/`\\` escaped; the
    /// root path is written as `.`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.segments.is_empty() {
            return f.write_str(".");
        }
        for (i, segment) in self.segments.iter().enumerate() {
            match segment {
                PathSegment::Index(n) => write!(f, "[{n}]")?,
                PathSegment::Key(key) => {
                    if i != 0 {
                        f.write_str(".")?;
                    }
                    if needs_quotes(key) {
                        f.write_str("\"")?;
                        for c in key.chars() {
                            match c {
                                '"' => f.write_str("\\\"")?,
                                '\\' => f.write_str("\\\\")?,
                                c if c.is_control() => write!(f, "\\u{{{:x}}}", u32::from(c))?,
                                c => write!(f, "{c}")?,
                            }
                        }
                        f.write_str("\"")?;
                    } else {
                        f.write_str(key)?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn display() {
        assert_eq!(Path::new().to_string(), ".");
        let p = Path::new()
            .with_key("spec")
            .with_key("ports")
            .with_index(2)
            .with_key("port");
        assert_eq!(p.to_string(), "spec.ports[2].port");
        assert_eq!(Path::new().with_index(0).to_string(), "[0]");
        assert_eq!(
            Path::new()
                .with_key("a.b")
                .with_key("")
                .with_key("q\"\\\n")
                .to_string(),
            "\"a.b\".\"\".\"q\\\"\\\\\\u{a}\""
        );
    }

    #[test]
    fn path_macro() {
        let key = String::from("k");
        let p = crate::path!["a", 0, &key, key.clone(), 3usize, 4u32, 5u64, -1];
        assert_eq!(
            p.segments(),
            &[
                PathSegment::Key("a".into()),
                PathSegment::Index(0),
                PathSegment::Key("k".into()),
                PathSegment::Key("k".into()),
                PathSegment::Index(3),
                PathSegment::Index(4),
                PathSegment::Index(5),
                PathSegment::Index(usize::MAX),
            ]
        );
        assert_eq!(crate::path![], Path::new());
        assert_eq!(crate::path!["x",], Path::new().with_key("x"));
    }

    #[test]
    fn parse_round_trips_display() {
        let keys = [
            "a", "spec", "a.b", "", "q\"\\\n", "[0]", "a]", "x y", "\u{85}", "été", "1", "-", "\t",
            "\\", "\"",
        ];
        for k in keys {
            for p in [
                Path::new().with_key(k),
                Path::new().with_key(k).with_index(3).with_key(k),
                Path::new().with_index(0).with_key(k).with_key("z"),
                Path::new().with_index(1).with_index(22),
            ] {
                assert_eq!(Path::parse(&p.to_string()), Ok(p.clone()), "{p}");
                assert_eq!(p.to_string().parse::<Path>(), Ok(p));
            }
        }
        assert_eq!(Path::parse("."), Ok(Path::new()));
        assert_eq!(Path::parse(&Path::new().to_string()), Ok(Path::new()));
        assert_eq!(
            Path::parse("spec.template.containers[0].image"),
            Ok(crate::path!["spec", "template", "containers", 0, "image"])
        );
        assert_eq!(Path::parse("a.\"b.c\"[0]"), Ok(crate::path!["a", "b.c", 0]));
        assert_eq!(Path::parse("a.1"), Ok(crate::path!["a", "1"]));
        assert_eq!(Path::parse("[007]"), Ok(crate::path![7]));
        assert_eq!(Path::parse("\"\\u{1F600}\""), Ok(crate::path!["\u{1F600}"]));
    }

    #[test]
    fn parse_errors() {
        let cases: [(&str, usize); 23] = [
            ("", 0),
            ("a..b", 2),
            ("a.", 2),
            (".a", 0),
            ("..", 0),
            ("a b", 1),
            ("a]", 1),
            ("]", 0),
            ("a[", 1),
            ("a[]", 1),
            ("a[x]", 2),
            ("a[-1]", 2),
            ("a[1", 1),
            ("a[99999999999999999999999]", 1),
            ("\"a", 0),
            ("\"a\"b", 3),
            ("[0]x", 3),
            ("\"\\x\"", 1),
            ("\"\\u{110000}\"", 1),
            ("\"\\u{}\"", 1),
            ("\"\\u{1234567}\"", 1),
            ("\"\\u41\"", 1),
            (" a", 0),
        ];
        for (s, offset) in cases {
            let e = Path::parse(s).unwrap_err();
            assert_eq!(e.offset(), offset, "{s:?}: {e}");
            assert!(!e.reason().is_empty());
            assert!(e.to_string().starts_with("invalid path at byte "));
        }
    }

    #[test]
    fn push_pop_and_accessors() {
        let mut p = Path::new();
        assert!(p.is_empty());
        p.push_key("a");
        p.push_index(3);
        assert_eq!(p.len(), 2);
        assert_eq!(
            p.segments(),
            &[PathSegment::Key("a".into()), PathSegment::Index(3)]
        );
        assert_eq!(p.pop(), Some(PathSegment::Index(3)));
        assert_eq!(p.pop(), Some(PathSegment::Key("a".into())));
        assert_eq!(p.pop(), None);
        assert_eq!(p, Path::default());
    }
}
