//! The paths the `Document` API's path methods take: a
//! [`Path`] or a path string, through one sealed trait.

use std::borrow::Cow;

use oneyaml_core::{ParsePathError, Path};

/// A path for [`Document::get_path`](crate::Document::get_path),
/// [`Document::set_path`](crate::Document::set_path) and the other path
/// methods: a [`Path`] (built with [`path!`](crate::path)), or a string in
/// the syntax of [`Path::parse`], such as
/// `"spec.template.containers[0].image"`.
///
/// Implemented for [`Path`], `str` and `String` (and references to each).
/// Sealed.
///
/// String syntax, in short: keys separated by `.`, `[n]` for sequence
/// index `n`, and a key holding `.`, `[`, `]`, `"`, `\`, white space, or
/// nothing in double quotes, with `\"`, `\\` and `\u{hex}` escaped:
/// `metadata.annotations."app.kubernetes.io/name"`. `.` is the root;
/// the empty string is malformed, so an empty key taken from user input
/// never names (and `set_path` never replaces) the whole document. Quote
/// keys that hold a `.`: unquoted, `labels.app.kubernetes.io/name` is
/// three nested keys below `labels` (`app`, `kubernetes`, `io/name`), and
/// [`Document::set_path`](crate::Document::set_path) would create them.
/// [`path!`](crate::path) builds the same [`Path`] from parts, with no
/// escaping.
///
/// Path keys are text: they match string keys. A key that is not a string
/// (the integer `80` in `80: http`) is reached with
/// [`NodeRef::get`](crate::NodeRef::get) or
/// [`NodeMut::get_mut`](crate::NodeMut::get_mut), which take any key.
///
/// A malformed path string names nothing: the read lookups return `None`,
/// the edit methods [`EditError::InvalidPath`](crate::EditError::InvalidPath).
///
/// ```
/// use oneyaml::{Document, path};
///
/// let doc = Document::parse("a:\n  b.c: [x, y]\n").unwrap();
/// assert_eq!(doc.get_path("a.\"b.c\"[1]").unwrap().text(), "y");
/// assert_eq!(doc.get_path(&path!["a", "b.c", 1]).unwrap().text(), "y");
/// assert!(doc.get_path("a.[1]").is_none()); // malformed: no node
/// assert!(doc.get_path(".").is_some()); // the root
/// assert!(doc.get_path("").is_none()); // empty: malformed, not the root
/// ```
pub trait IntoPath: sealed::Sealed {
    // Sealed: no methods of its own; the path a type gives is internal.
}

mod sealed {
    use super::{Cow, ParsePathError, Path};

    pub trait Sealed {
        fn path_arg(&self) -> Result<Cow<'_, Path>, ParsePathError>;
    }
}

impl IntoPath for Path {}
impl sealed::Sealed for Path {
    fn path_arg(&self) -> Result<Cow<'_, Path>, ParsePathError> {
        Ok(Cow::Borrowed(self))
    }
}

impl IntoPath for str {}
impl sealed::Sealed for str {
    fn path_arg(&self) -> Result<Cow<'_, Path>, ParsePathError> {
        Path::parse(self).map(Cow::Owned)
    }
}

impl IntoPath for String {}
impl sealed::Sealed for String {
    fn path_arg(&self) -> Result<Cow<'_, Path>, ParsePathError> {
        Path::parse(self).map(Cow::Owned)
    }
}

impl<T: IntoPath + ?Sized> IntoPath for &T {}
impl<T: sealed::Sealed + ?Sized> sealed::Sealed for &T {
    fn path_arg(&self) -> Result<Cow<'_, Path>, ParsePathError> {
        (**self).path_arg()
    }
}

/// The path `p` names, for a read lookup (`None` when malformed).
pub(crate) fn read<P: IntoPath + ?Sized>(p: &P) -> Option<Cow<'_, Path>> {
    sealed::Sealed::path_arg(p).ok()
}

/// The path `p` names, for an edit.
///
/// # Errors
///
/// [`EditError::InvalidPath`](super::EditError::InvalidPath) when
/// malformed.
pub(crate) fn edit<P: IntoPath + ?Sized>(p: &P) -> Result<Cow<'_, Path>, super::EditError> {
    sealed::Sealed::path_arg(p).map_err(|e| super::EditError::InvalidPath {
        offset: e.offset(),
        reason: e.reason(),
    })
}
