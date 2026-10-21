//! [`LineIndex`]: UTF-8 byte offsets to lines and columns.

/// The unit a column is counted in.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnUnit {
    /// UTF-8 bytes (what [`crate::Span`]s count).
    Utf8,
    /// Unicode scalar values (`char`s), as editors usually show them.
    Char,
    /// UTF-16 code units (the Language Server Protocol's default).
    Utf16,
}

/// A 0-based line and column; the column is in the [`ColumnUnit`] asked
/// for.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineCol {
    /// 0-based line.
    pub line: usize,
    /// 0-based column.
    pub column: usize,
}

impl LineCol {
    /// A 0-based line and column (`#[non_exhaustive]`: fields may be
    /// added, so build it here).
    #[must_use]
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// A multi-byte character: where it starts, its UTF-8 length, and the
/// running totals up to and including it of the bytes it has beyond one
/// `char` and beyond its UTF-16 length.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Wide {
    at: usize,
    len: usize,
    extra_chars: usize,
    extra_utf16: usize,
}

/// Line starts of one source text, to turn UTF-8 byte offsets (the unit
/// of every [`crate::Span`]) into line/column positions in UTF-8 bytes,
/// `char`s, or UTF-16 code units.
///
/// Built once in O(n); each lookup is O(log n) (binary searches over the
/// line starts and the multi-byte characters), whatever the line length.
/// Line breaks are `\n`, `\r\n`, and a lone `\r` (YAML 1.2.2 §5.4). A byte
/// order mark at the very start of the text is not a character of the
/// document (§5.2): columns counted in `char`s or UTF-16 units on the first
/// line leave it out, so the first character after it is column 0. Byte
/// columns count it, because byte offsets do. An
/// offset past the end, or inside a multi-byte character, has no position
/// (`None`); nothing here panics.
///
/// ```
/// use oneyaml_core::{ColumnUnit, LineCol, LineIndex};
///
/// let src = "a: 1\nb: \u{e9}\u{1F600}x\n";
/// let idx = LineIndex::new(src);
/// let x = src.find('x').unwrap();
/// assert_eq!(idx.line_col(x, ColumnUnit::Utf8), Some(LineCol::new(1, 9)));
/// assert_eq!(idx.line_col(x, ColumnUnit::Char), Some(LineCol::new(1, 5)));
/// assert_eq!(idx.line_col(x, ColumnUnit::Utf16), Some(LineCol::new(1, 6)));
/// assert_eq!(idx.line_col(x - 1, ColumnUnit::Char), None); // inside the emoji
/// assert_eq!(idx.line_col(src.len() + 1, ColumnUnit::Char), None);
/// // A text ending in a line break has an empty last line.
/// assert_eq!(idx.line_count(), 3);
/// assert_eq!(idx.line_start(1), Some(5));
/// assert_eq!(idx.line_start(3), None);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LineIndex {
    /// Byte offset of each line's start; the first is 0.
    starts: Vec<usize>,
    /// Every multi-byte character, in order.
    wide: Vec<Wide>,
    len: usize,
    /// The text starts with a byte order mark.
    bom: bool,
}

impl LineIndex {
    /// Indexes `src`.
    #[must_use]
    pub fn new(src: &str) -> Self {
        let mut starts = vec![0];
        let mut wide = Vec::new();
        let (mut extra_chars, mut extra_utf16) = (0usize, 0usize);
        let mut chars = src.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            let len = c.len_utf8();
            let next = at.saturating_add(len);
            match c {
                '\n' => starts.push(next),
                '\r' if !matches!(chars.peek(), Some((_, '\n'))) => starts.push(next),
                _ if len > 1 => {
                    extra_chars = extra_chars.saturating_add(len.saturating_sub(1));
                    extra_utf16 = extra_utf16.saturating_add(len.saturating_sub(c.len_utf16()));
                    wide.push(Wide {
                        at,
                        len,
                        extra_chars,
                        extra_utf16,
                    });
                }
                _ => {}
            }
        }
        Self {
            starts,
            wide,
            len: src.len(),
            bom: src.starts_with('\u{feff}'),
        }
    }

    /// The length of the indexed text, in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// `true` for an empty text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of lines (a text ending in a line break has an empty
    /// last line).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The byte offset where 0-based line `line` starts.
    #[must_use]
    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.starts.get(line).copied()
    }

    /// The running totals of extra bytes (per `char`, per UTF-16 unit) of
    /// the multi-byte characters that end at or before `offset`; `None`
    /// when `offset` falls inside one.
    fn extra_before(&self, offset: usize) -> Option<(usize, usize)> {
        let n = self.wide.partition_point(|w| w.at < offset);
        let Some(last) = n.checked_sub(1).and_then(|i| self.wide.get(i)) else {
            return Some((0, 0));
        };
        if offset < last.at.saturating_add(last.len) {
            return None;
        }
        Some((last.extra_chars, last.extra_utf16))
    }

    /// The position of byte `offset`, its column counted in `unit`;
    /// `None` past the end or inside a multi-byte character. O(log n).
    #[must_use]
    pub fn line_col(&self, offset: usize, unit: ColumnUnit) -> Option<LineCol> {
        if offset > self.len {
            return None;
        }
        let line = self
            .starts
            .partition_point(|s| *s <= offset)
            .checked_sub(1)?;
        let start = *self.starts.get(line)?;
        let bytes = offset.checked_sub(start)?;
        let (at_chars, at_utf16) = self.extra_before(offset)?;
        let (start_chars, start_utf16) = self.extra_before(start)?;
        let column = match unit {
            ColumnUnit::Char => bytes.checked_sub(at_chars.checked_sub(start_chars)?)?,
            ColumnUnit::Utf16 => bytes.checked_sub(at_utf16.checked_sub(start_utf16)?)?,
            ColumnUnit::Utf8 => bytes,
        };
        // The mark is one character and one UTF-16 unit wide.
        let column = if self.bom && line == 0 && unit != ColumnUnit::Utf8 && offset > 0 {
            column.saturating_sub(1)
        } else {
            column
        };
        Some(LineCol { line, column })
    }

    /// The positions of a span's start and end (see
    /// [`LineIndex::line_col`]).
    #[must_use]
    pub fn span(&self, span: crate::Span, unit: ColumnUnit) -> Option<(LineCol, LineCol)> {
        Some((
            self.line_col(span.start, unit)?,
            self.line_col(span.end, unit)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]
    use super::*;

    /// The naive answer, by walking the text.
    fn naive(src: &str, offset: usize, unit: ColumnUnit) -> Option<LineCol> {
        let before = src.get(..offset)?;
        let (mut line, mut column) = (0usize, 0usize);
        let mut prev_cr = false;
        for c in before.chars() {
            match c {
                '\n' if prev_cr => {
                    column = 0;
                }
                '\n' => {
                    line += 1;
                    column = 0;
                }
                '\r' => {
                    line += 1;
                    column = 0;
                }
                _ => {
                    column += match unit {
                        ColumnUnit::Utf8 => c.len_utf8(),
                        ColumnUnit::Char => 1,
                        ColumnUnit::Utf16 => c.len_utf16(),
                    }
                }
            }
            prev_cr = c == '\r';
        }
        Some(LineCol { line, column })
    }

    #[test]
    fn agrees_with_a_walk_everywhere() {
        let src = "a\r\nb\rc\nd\u{e9}x\u{1F600}\u{4e2d}\r\n\n\u{e9}";
        let idx = LineIndex::new(src);
        assert_eq!(idx.len(), src.len());
        assert!(!idx.is_empty());
        assert_eq!(idx.line_count(), 6);
        for offset in 0..=src.len() + 2 {
            for unit in [ColumnUnit::Utf8, ColumnUnit::Char, ColumnUnit::Utf16] {
                // The `\n` of a `\r\n` belongs to the line it ends.
                let crlf_nl = src.get(offset..).is_some_and(|r| r.starts_with('\n'))
                    && src.get(..offset).is_some_and(|r| r.ends_with('\r'));
                if crlf_nl {
                    let at = idx.line_col(offset, unit).unwrap();
                    let before = idx.line_col(offset - 1, unit).unwrap();
                    assert_eq!(at.line, before.line);
                    assert_eq!(at.column, before.column + 1);
                    continue;
                }
                assert_eq!(
                    idx.line_col(offset, unit),
                    naive(src, offset, unit),
                    "{offset} {unit:?}"
                );
            }
        }
        assert_eq!(idx.line_start(1), Some(3));
        assert_eq!(idx.line_start(99), None);
        let e = src.find('x').unwrap();
        assert_eq!(
            idx.span(crate::Span::new(e, e + 1), ColumnUnit::Char),
            Some((
                LineCol { line: 3, column: 2 },
                LineCol { line: 3, column: 3 }
            ))
        );
        assert_eq!(idx.span(crate::Span::new(0, 999), ColumnUnit::Char), None);
    }

    #[test]
    fn a_leading_byte_order_mark_is_not_a_column() {
        let src = "\u{feff}a: \u{e9}x\n\u{feff}b\n";
        let idx = LineIndex::new(src);
        let x = src.find('x').unwrap();
        // Bytes count the mark; characters and UTF-16 units do not.
        assert_eq!(idx.line_col(x, ColumnUnit::Utf8), Some(LineCol::new(0, 8)));
        assert_eq!(idx.line_col(x, ColumnUnit::Char), Some(LineCol::new(0, 4)));
        assert_eq!(idx.line_col(x, ColumnUnit::Utf16), Some(LineCol::new(0, 4)));
        // The first character after the mark is column 0; the mark itself too.
        assert_eq!(idx.line_col(3, ColumnUnit::Char), Some(LineCol::new(0, 0)));
        assert_eq!(idx.line_col(0, ColumnUnit::Char), Some(LineCol::new(0, 0)));
        // Only a mark at the start of the text is left out.
        let second = src.rfind('b').unwrap();
        assert_eq!(
            idx.line_col(second, ColumnUnit::Char),
            Some(LineCol::new(1, 1))
        );
    }

    #[test]
    fn empty_and_ascii() {
        let idx = LineIndex::new("");
        assert!(idx.is_empty());
        assert_eq!(
            idx.line_col(0, ColumnUnit::Utf16),
            Some(LineCol { line: 0, column: 0 })
        );
        assert_eq!(idx.line_col(1, ColumnUnit::Utf16), None);
        let idx = LineIndex::new("ab\n");
        assert_eq!(
            idx.line_col(3, ColumnUnit::Char),
            Some(LineCol { line: 1, column: 0 })
        );
        assert_eq!(idx.line_col(usize::MAX, ColumnUnit::Char), None);
    }
}
