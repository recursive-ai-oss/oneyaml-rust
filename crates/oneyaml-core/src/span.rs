//! Source spans.

/// A UTF-8 byte range `start..end` into the source text.
///
/// ```
/// use oneyaml_core::Span;
///
/// let src = "key: value";
/// let s = Span::new(5, 10);
/// assert_eq!((&src[s.start..s.end], s.len()), ("value", 5));
/// assert!(Span::empty(5).is_empty()); // an insertion point
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    /// First byte of the range.
    pub start: usize,
    /// One past the last byte of the range.
    pub end: usize,
}

impl Span {
    /// Creates the span `start..end`.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// A zero-width span at `at`.
    #[must_use]
    pub const fn empty(at: usize) -> Self {
        Self { start: at, end: at }
    }

    /// Length in bytes; `0` for an inverted span.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// `true` when the span covers no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_and_empty() {
        assert_eq!(Span::new(2, 5).len(), 3);
        assert!(Span::empty(4).is_empty());
        assert_eq!(Span::empty(4), Span::new(4, 4));
        assert_eq!(Span::new(5, 2).len(), 0);
        assert!(!Span::new(0, 1).is_empty());
    }
}
