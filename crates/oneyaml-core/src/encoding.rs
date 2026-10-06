//! Character encodings of YAML input (YAML 1.2.2 §5.2), as vocabulary
//! shared by the decoder (`oneyaml-syntax`) and `Document::encoding`
//! (`oneyaml`), so the stable API names no syntax type.
//! Only the names live here; detection and decoding live in
//! `oneyaml-syntax`.

/// A character encoding of YAML input (§5.2).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// UTF-8 (the default when nothing else is detected).
    Utf8,
    /// UTF-16, little-endian.
    Utf16Le,
    /// UTF-16, big-endian.
    Utf16Be,
    /// UTF-32, little-endian.
    Utf32Le,
    /// UTF-32, big-endian.
    Utf32Be,
}

impl Encoding {
    /// The encoding's usual name (`UTF-8`, `UTF-16LE`, …).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Utf32Le => "UTF-32LE",
            Self::Utf32Be => "UTF-32BE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Encoding;

    #[test]
    fn names() {
        assert_eq!(Encoding::Utf8.name(), "UTF-8");
        assert_eq!(Encoding::Utf16Le.name(), "UTF-16LE");
        assert_eq!(Encoding::Utf16Be.name(), "UTF-16BE");
        assert_eq!(Encoding::Utf32Le.name(), "UTF-32LE");
        assert_eq!(Encoding::Utf32Be.name(), "UTF-32BE");
    }
}
