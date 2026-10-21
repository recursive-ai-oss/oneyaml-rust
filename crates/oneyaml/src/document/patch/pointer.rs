//! RFC 6901 JSON Pointers.

use core::fmt;
use core::str::FromStr;

use super::{PatchError, PatchErrorKind};

/// An RFC 6901 JSON Pointer: a list of reference tokens.
///
/// The empty pointer names the whole document. `~1` in text stands for `/`
/// and `~0` for `~`; a token is always stored unescaped.
///
/// ```
/// use oneyaml::Pointer;
///
/// let p = Pointer::parse("/a~1b/~0c/0").unwrap();
/// assert_eq!(p.tokens(), ["a/b", "~c", "0"]);
/// assert_eq!(p.to_string(), "/a~1b/~0c/0");
/// assert!(Pointer::parse("").unwrap().is_root());
/// assert!(Pointer::parse("a").is_err()); // must start with `/`
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Pointer {
    tokens: Vec<String>,
}

impl Pointer {
    /// The empty pointer, naming the whole document.
    #[must_use]
    pub fn root() -> Self {
        Self::default()
    }

    /// Reads pointer text.
    ///
    /// # Errors
    ///
    /// [`PatchErrorKind::Invalid`] when the text is not empty and does not
    /// start with `/`, or has a `~` that is not followed by `0` or `1`. Linear
    /// in the text's length, whatever it holds.
    pub fn parse(text: &str) -> Result<Self, PatchError> {
        if text.is_empty() {
            return Ok(Self::root());
        }
        let Some(rest) = text.strip_prefix('/') else {
            return Err(PatchError::new(
                PatchErrorKind::Invalid,
                "a JSON Pointer is empty or starts with `/`",
            )
            .at(text));
        };
        let mut tokens = Vec::new();
        for raw in rest.split('/') {
            tokens.push(unescape(raw).ok_or_else(|| {
                PatchError::new(
                    PatchErrorKind::Invalid,
                    "a `~` in a JSON Pointer must be followed by `0` or `1`",
                )
                .at(text)
            })?);
        }
        Ok(Self { tokens })
    }

    /// A pointer from unescaped tokens, and one more token added with
    /// [`Pointer::with`]. A token that holds `/` or `~` needs no escaping here.
    ///
    /// ```
    /// use oneyaml::{Document, Pointer};
    ///
    /// let pointer = Pointer::from_tokens(["a/b", "items"]).with("1");
    /// assert_eq!(pointer.to_string(), "/a~1b/items/1");
    /// let doc = Document::parse("a/b:\n  items: [x, y]\n").unwrap();
    /// let node = doc.get_pointer_tokens(&pointer).unwrap();
    /// assert_eq!(node.as_str().as_deref(), Some("y"));
    /// ```
    #[must_use]
    pub fn from_tokens<I, S>(tokens: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            tokens: tokens.into_iter().map(Into::into).collect(),
        }
    }

    /// The unescaped tokens, outermost first.
    #[must_use]
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    /// `true` for the empty pointer.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.tokens.is_empty()
    }

    /// This pointer with one more token.
    #[must_use]
    pub fn with(mut self, token: impl Into<String>) -> Self {
        self.tokens.push(token.into());
        self
    }
}

/// `raw` with `~1` and `~0` decoded, or `None` for a bad escape.
fn unescape(raw: &str) -> Option<String> {
    if !raw.contains('~') {
        return Some(raw.to_string());
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '~' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => out.push('~'),
            Some('1') => out.push('/'),
            _ => return None,
        }
    }
    Some(out)
}

impl fmt::Display for Pointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for t in &self.tokens {
            f.write_str("/")?;
            for c in t.chars() {
                match c {
                    '~' => f.write_str("~0")?,
                    '/' => f.write_str("~1")?,
                    c => fmt::Write::write_char(f, c)?,
                }
            }
        }
        Ok(())
    }
}

impl FromStr for Pointer {
    type Err = PatchError;

    fn from_str(s: &str) -> Result<Self, PatchError> {
        Self::parse(s)
    }
}
