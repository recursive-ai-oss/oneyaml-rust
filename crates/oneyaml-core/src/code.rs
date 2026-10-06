//! Stable `Y####` diagnostic codes and the single code registry.
//!
//! # Allocation rules
//!
//! | Range         | Area                                                        |
//! |---------------|-------------------------------------------------------------|
//! | `Y0001–Y0999` | syntax: decode, lex, parse, recovery                        |
//! | `Y1000–Y1999` | schema & resolution: directives, tags, duplicate keys, settings conflicts |
//! | `Y2000–Y2999` | limits                                                      |
//! | `Y3000–Y3999` | serde                                                       |
//! | `Y9000–Y9999` | reserved; `Y9000` = internal error                   |
//!
//! `Y0000` is never used. Codes are allocated **sequentially within their
//! range**, only in this file: a new code is `max + 1` of its range, added as
//! a constant plus a matching [`REGISTRY`] entry.
//!
//! **The registry is append-only.** A code is never removed, reused,
//! renumbered, renamed, or given a new meaning. To retire a code, set
//! [`CodeInfo::deprecated`]; the entry stays forever, so ranges stay
//! contiguous. `crates/oneyaml-core/tests/codes.snapshot` freezes every
//! entry (code, name, severity, status, summary) and a test compares the
//! registry with it, so any change to an existing entry shows up as a snapshot
//! diff in review.

use crate::Severity;

/// A stable diagnostic code, rendered as `Y####`.
///
/// ```
/// use oneyaml_core::code::{self, CodeRange};
/// use oneyaml_core::{Code, Severity};
///
/// let c = Code::parse("Y1014").unwrap();
/// assert_eq!(c, code::DUPLICATE_KEY_LAST_WINS);
/// assert_eq!((c.get(), c.to_string()), (1014, "Y1014".to_owned()));
/// assert_eq!(c.range(), CodeRange::Schema);
/// assert_eq!(CodeRange::Schema.bounds(), (1000, 1999));
/// let info = c.info().unwrap(); // the registry entry
/// assert_eq!(info.name, "DUPLICATE_KEY_LAST_WINS");
/// assert_eq!(info.severity, Severity::Warning);
/// assert!(Code::parse("Y0000").is_none()); // Y0000 is never used
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Code(u16);

/// The area a [`Code`] belongs to.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeRange {
    /// `Y0001–Y0999`: decode, lex, parse, recovery.
    Syntax,
    /// `Y1000–Y1999`: directives, tags, duplicate keys, settings conflicts.
    Schema,
    /// `Y2000–Y2999`: resource limits.
    Limits,
    /// `Y3000–Y3999`: serde.
    Serde,
    /// `Y9000–Y9999`: reserved. `Y9000` is the internal error.
    Reserved,
}

impl CodeRange {
    /// The inclusive numeric bounds of this range.
    #[must_use]
    pub const fn bounds(self) -> (u16, u16) {
        match self {
            Self::Syntax => (1, 999),
            Self::Schema => (1000, 1999),
            Self::Limits => (2000, 2999),
            Self::Serde => (3000, 3999),
            Self::Reserved => (9000, 9999),
        }
    }
}

impl Code {
    /// Returns the code with number `n`, or `None` if `n` lies outside every
    /// allocated range (including `0`).
    ///
    /// This validates the *range* only and exists for parsing, deserializing,
    /// and tests. Library code never calls it: it emits only the registered
    /// constants in this module, which a test enforces by scanning the
    /// library crates' sources.
    #[must_use]
    pub const fn new(n: u16) -> Option<Self> {
        match n {
            1..=3999 | 9000..=9999 => Some(Self(n)),
            _ => None,
        }
    }

    /// The numeric value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The range this code belongs to.
    #[must_use]
    pub const fn range(self) -> CodeRange {
        match self.0 {
            0..=999 => CodeRange::Syntax,
            1000..=1999 => CodeRange::Schema,
            2000..=2999 => CodeRange::Limits,
            3000..=3999 => CodeRange::Serde,
            _ => CodeRange::Reserved,
        }
    }

    /// Parses the `Y####` form (exactly four digits).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let digits = s.strip_prefix('Y')?;
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Self::new(digits.parse().ok()?)
    }

    /// The registry entry for this code, if it is allocated.
    #[must_use]
    pub fn info(self) -> Option<&'static CodeInfo> {
        REGISTRY.iter().find(|info| info.code == self)
    }
}

impl core::fmt::Display for Code {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Y{:04}", self.0)
    }
}

/// One registry entry.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CodeInfo {
    /// The code.
    pub code: Code,
    /// The constant's name in this module.
    pub name: &'static str,
    /// The severity diagnostics with this code are emitted at by default.
    pub severity: Severity,
    /// One-line meaning. Never changes once published.
    pub summary: &'static str,
    /// Retired: never emitted any more, but never removed or reused.
    pub deprecated: bool,
}

// ---------------------------------------------------------------------------
// Y0001–Y0999: syntax.
// ---------------------------------------------------------------------------

/// Directives not followed by an explicit `---` (productions 210-211). Error.
pub const DIRECTIVE_WITHOUT_DOCUMENT: Code = Code(1);
/// A `%YAML`/`%TAG` directive with missing, extra, or malformed parameters,
/// or a directive without a name (§6.8). Error.
pub const MALFORMED_DIRECTIVE: Code = Code(2);
/// A directive inside a document that was not ended with `...`
/// (production 211). Error.
pub const DIRECTIVE_IN_DOCUMENT: Code = Code(3);
/// Content after a document's root node, or after a `...` marker on the same
/// line (§9.1). Error.
pub const CONTENT_AFTER_DOCUMENT: Code = Code(4);
/// A line indented so that it belongs to no open block collection (§8.2).
/// Error.
pub const INVALID_INDENTATION: Code = Code(5);
/// A block sequence or mapping starting on a line where only a scalar may
/// start, e.g. after `---`, after an implicit key's `:`, or after properties
/// (production 200). Error.
pub const BLOCK_COLLECTION_NOT_ALLOWED: Code = Code(6);
/// An implicit mapping key spanning more than one line (production 154).
/// Error.
pub const MULTILINE_IMPLICIT_KEY: Code = Code(7);
/// A character that cannot start a node (§7.3.3, production 126). Error.
pub const INVALID_NODE_START: Code = Code(8);
/// A quoted scalar not closed before the end of input or a document marker
/// (§7.3). Error.
pub const UNTERMINATED_QUOTED_SCALAR: Code = Code(9);
/// An invalid escape sequence in a double-quoted scalar (§5.7). Error.
pub const INVALID_ESCAPE: Code = Code(10);
/// A continuation line of a quoted scalar indented too little
/// (production 69). Error.
pub const QUOTED_SCALAR_INDENTATION: Code = Code(11);
/// An empty or malformed anchor or alias name (production 102). Error.
pub const INVALID_ANCHOR: Code = Code(12);
/// A malformed tag property (§6.9.1). Error.
pub const INVALID_TAG: Code = Code(13);
/// An alias node with an anchor or tag (production 104). Error.
pub const ALIAS_WITH_PROPERTIES: Code = Code(14);
/// A node with two anchors or two tags (production 96). Error.
pub const DUPLICATE_PROPERTY: Code = Code(15);
/// A tab character used as block indentation (§6.1). Error.
pub const TAB_INDENTATION: Code = Code(16);
/// Unexpected content after a complete node on the same line. Error.
pub const UNEXPECTED_CONTENT: Code = Code(17);
/// A block mapping entry without its `:` value indicator (production 192).
/// Error.
pub const MISSING_MAPPING_COLON: Code = Code(18);
/// Syntax this parser version does not implement yet. **Deprecated**: used
/// only for flow collections and block scalars by pre-release parsers
/// that did not implement them; never emitted any more. Error.
pub const UNSUPPORTED_SYNTAX: Code = Code(19);
/// A malformed block scalar header: an indentation indicator of `0`, a
/// repeated indicator, or text after the header (production 162). Error.
pub const INVALID_BLOCK_HEADER: Code = Code(20);
/// A leading empty line of an auto-indented block scalar has more spaces
/// than its first content line (§8.1.1.1). Error.
pub const BLOCK_SCALAR_INDENTATION: Code = Code(21);
/// A flow collection not closed before the end of input or a document
/// marker (§7.4). Error.
pub const UNTERMINATED_FLOW_COLLECTION: Code = Code(22);
/// Two flow collection entries without a `,` between them (productions 138,
/// 141). Error.
pub const MISSING_FLOW_SEPARATOR: Code = Code(23);
/// A `,`, `]`, or `}` where it cannot appear, e.g. a leading or doubled
/// `,`, or a mismatched closing bracket (§7.4). Error.
pub const UNEXPECTED_FLOW_INDICATOR: Code = Code(24);
/// An implicit key (with its properties and trailing white space) longer
/// than 1024 characters (productions 154-155, §8.2.2). Error.
pub const IMPLICIT_KEY_TOO_LONG: Code = Code(25);
/// A character outside `nb-json` (production 2), i.e. a C0 control other
/// than tab, inside a quoted scalar (§7.3). Error.
pub const INVALID_QUOTED_CHARACTER: Code = Code(26);
/// A `: ` inside an unquoted (plain) scalar that is the value of a block
/// mapping entry, on the entry's line, e.g. `title: The farm: a banana
/// story`. The `: ` would start a nested mapping, which cannot begin on
/// that line (production 200). A more specific diagnosis of the case that
/// earlier parser versions reported as [`BLOCK_COLLECTION_NOT_ALLOWED`];
/// carries a machine-applicable fix that quotes the value. Error.
pub const INLINE_COLON_IN_PLAIN_VALUE: Code = Code(27);
/// The input of [`INLINE_COLON_IN_PLAIN_VALUE`] was read as one string
/// because the `lenient_inline_colons` setting is on. Warning.
pub const LENIENT_INLINE_COLON: Code = Code(28);
/// Bytes that are not valid UTF-8 in input decoded as UTF-8 (§5.2). Each
/// maximal invalid run is replaced by U+FFFD. Error.
pub const INVALID_UTF8: Code = Code(29);
/// Input decoded as UTF-16 with an unpaired surrogate or a trailing odd byte
/// (§5.2). Each maximal invalid run is replaced by U+FFFD. Error.
pub const INVALID_UTF16: Code = Code(30);
/// Input decoded as UTF-32 with a code unit that is not a Unicode scalar
/// value, or trailing bytes that do not form a code unit (§5.2). Each
/// maximal invalid run is replaced by U+FFFD. Error.
pub const INVALID_UTF32: Code = Code(31);
/// A byte order mark (U+FEFF) where YAML 1.2.2 does not allow one (§5.2,
/// productions 27, 202, 211). A mark is allowed only at the very start of
/// a line that starts a document prefix: the start of the stream, after a
/// `...` line, or after a complete document (whose end it then marks; only
/// a document starting with `---` may follow it). It is also allowed
/// inside quoted scalars, whose `nb-json` characters include it.
///
/// Reported for a mark after white space or other text on a line, after
/// directives, inside a document's structure, and inside comment text or
/// block scalar content (`nb-char` excludes it). Recovery: a mark between
/// tokens is skipped, and columns are counted as if it were absent; a
/// mark inside a comment or a block scalar stays in that token (the
/// diagnostic's span is the mark; the block scalar's value keeps it). A
/// mark ends a plain scalar (it is no `ns-char`), so text right after it
/// is read as new content (usually a further error). A mark between an
/// implicit key and its `:` is skipped like any other.
///
/// When a mark makes another construct fail (a key without `:`, a block
/// scalar header, a block scalar's offending line, a directive, or any
/// other syntax error whose offending line holds a mark before any quote
/// character), this code is reported too, right after that construct's
/// error (whose label comes first on the line), so the construct's error
/// is the one a strict parse fails with.
/// Not covered: a mark after a quote character on such a line (it may be
/// inside a quoted scalar, where it is allowed), which then shows only
/// through the other error.
///
/// A mark at the start of a line after a complete document is valid and
/// ends that document; if what follows is not a document starting with
/// `---`, the error is `CONTENT_AFTER_DOCUMENT` (or
/// `DIRECTIVE_IN_DOCUMENT`), with a label on that mark, and the content is
/// read as the next document. Error.
pub const MISPLACED_BYTE_ORDER_MARK: Code = Code(32);

// ---------------------------------------------------------------------------
// Y1000–Y1999: schema & resolution.
// ---------------------------------------------------------------------------

/// A `%YAML 1.1` directive. By default the document is parsed as
/// YAML 1.2; under `honor_version_directive` the YAML 1.1 preset applies.
/// The message says which. Warning.
pub const YAML_VERSION_1_1: Code = Code(1000);
/// `%YAML 1.x` with `x > 2`: processed as YAML 1.2. Warning.
pub const YAML_VERSION_NEWER_MINOR: Code = Code(1001);
/// `%YAML` with a major version other than 1. Error.
pub const YAML_VERSION_UNSUPPORTED_MAJOR: Code = Code(1002);
/// A reserved directive (`%NAME` other than `YAML`/`TAG`) was ignored
/// (YAML 1.2.2 §6.8). Warning.
pub const RESERVED_DIRECTIVE_IGNORED: Code = Code(1003);
/// A second `%YAML` directive, or a `%TAG` handle declared twice, for one
/// document (§6.8.1, §6.8.2). Error.
pub const DUPLICATE_DIRECTIVE: Code = Code(1004);
/// A named tag handle (`!e!`) not declared by a `%TAG` directive of its
/// document (§6.8.2.1). Error.
pub const UNDEFINED_TAG_HANDLE: Code = Code(1005);
/// A `%YAML 1.0` directive. By default the document is parsed as
/// YAML 1.2; under `honor_version_directive` the YAML 1.1 preset applies,
/// as for 1.1. The message says which. Warning.
pub const YAML_VERSION_1_0: Code = Code(1006);
/// An explicitly tagged scalar whose text is not a valid form of its core
/// tag under the active schema, e.g. `!!int twelve` (§10). Error.
pub const INVALID_SCALAR_FOR_TAG: Code = Code(1007);
/// A collection tag (`!!seq`, `!!map`) on a scalar node (§10). Error.
pub const COLLECTION_TAG_ON_SCALAR: Code = Code(1008);
/// A core tag that does not match the collection it is on, e.g. `!!str [a]`
/// or `!!map [a]` (§10). Error.
pub const WRONG_TAG_ON_COLLECTION: Code = Code(1009);
/// JSON schema: a plain scalar matching none of the JSON patterns
/// (§10.2.2). Error.
pub const JSON_UNMATCHED_SCALAR: Code = Code(1010);
/// A combination of settings in which a setting has no effect.
/// Warning.
pub const MEANINGLESS_SETTINGS: Code = Code(1011);
/// A combination of settings that would produce incorrect results.
/// Error.
pub const INCORRECT_SETTINGS: Code = Code(1012);
/// A mapping key that is equal to an earlier key of the same mapping
/// (§3.2.1.1). Error.
pub const DUPLICATE_KEY: Code = Code(1013);
/// A duplicate mapping key tolerated by `DuplicateKeys::LastWins`: the
/// last value wins; a secondary label points at the first definition.
/// Warning.
pub const DUPLICATE_KEY_LAST_WINS: Code = Code(1014);
/// An alias whose anchor is not defined earlier in the document, or whose
/// anchored node is not complete yet (a recursive alias) (§7.1). Error.
pub const UNDEFINED_ALIAS: Code = Code(1015);
/// A merge key (`<<`, with `merge_keys` on) whose value is not a mapping or
/// a sequence of mappings (yaml.org/type/merge). Error.
pub const INVALID_MERGE_VALUE: Code = Code(1016);
/// A setting this version cannot honour, e.g. `Emit::line_width(Some(_))`
/// (line wrapping is not implemented): refused rather than silently ignored.
/// Error.
pub const UNSUPPORTED_SETTING: Code = Code(1017);

// ---------------------------------------------------------------------------
// Y2000–Y2999: limits. The help of every limit diagnostic names the
// exact setting to raise and its current value.
// ---------------------------------------------------------------------------

/// Collections nested deeper than `Limits::max_depth` (block and flow
/// nesting combined). Error.
pub const DEPTH_LIMIT_EXCEEDED: Code = Code(2000);
/// A scalar value longer than `Limits::max_scalar_len` bytes. Error.
pub const SCALAR_TOO_LONG: Code = Code(2001);
/// Input larger than `Limits::max_document_size` bytes. Error.
pub const DOCUMENT_TOO_LARGE: Code = Code(2002);
/// A stream with more documents than `Limits::max_documents`. Error.
pub const TOO_MANY_DOCUMENTS: Code = Code(2003);
/// Alias expansion (including merge keys through aliases) produced more
/// nodes than `Limits::alias_budget` allows; `Value` and serde only.
/// Error.
pub const ALIAS_BUDGET_EXCEEDED: Code = Code(2004);
/// A `Value` consumed by the library (serialized, or deserialized into a
/// type) nests deeper than `Limits::max_depth`. Error.
///
/// Reported by the serializer (`to_string`, `to_writer`, `Serializer`),
/// which emits `Value`s iteratively; `Document` insertion reports it too.
pub const VALUE_DEPTH_EXCEEDED: Code = Code(2005);
/// Input with more elements (nodes, tokens, trivia) than a syntax tree's
/// `u32` indices can address; the rest of the input is kept as skipped
/// bytes, so the tree still round-trips. Error.
pub const TREE_CAPACITY_EXCEEDED: Code = Code(2006);
/// Deserializing into a *recursive user type* (e.g. `struct Tree(Vec<Tree>)`)
/// nested deeper than `Limits::max_typed_depth`. serde's visitor
/// model recurses once per level of such a type, so this bounds the native
/// stack; `Value` targets are iterative and bounded only by `max_depth`.
/// Also `Serialize for Value` into third-party serializers, which recurse
/// once per level: values deeper than the default `max_typed_depth` (128)
/// are refused. Error.
pub const TYPED_DEPTH_EXCEEDED: Code = Code(2007);
/// A `Value` converted to a `serde_json::Value` (feature `serde_json`)
/// nests deeper than 128 levels. The bound is fixed, whatever `Limits`
/// say: serde_json's recursion limit, and its `Value` drops recursively.
/// Error.
pub const JSON_DEPTH_EXCEEDED: Code = Code(2008);
/// Alias expansion materialised more bytes than `Limits::alias_byte_budget`
/// allows: scalar text, keys, and tags copied by expansion (merge keys
/// through aliases included), each tag use's `%TAG` prefix, and `%TAG` tags
/// joined into enum variant names; `Value` and serde only.
/// Error.
pub const ALIAS_BYTE_BUDGET_EXCEEDED: Code = Code(2009);
/// More diagnostics than `Limits::max_diagnostics` were found: the list
/// ends with this one summary, "N more diagnostics suppressed", located
/// where the first suppressed one was. Not a problem of the
/// input by itself: a warning, so a strict parse still fails only at an
/// error. Warning.
pub const DIAGNOSTICS_SUPPRESSED: Code = Code(2010);

// ---------------------------------------------------------------------------
// Y3000–Y3999: serde.
// ---------------------------------------------------------------------------

/// A node's type does not fit the target type, e.g. a mapping for a
/// `String`, or a quoted scalar for a number. Error.
pub const TYPE_MISMATCH: Code = Code(3000);
/// An integer outside the range of the target integer type. Error.
pub const INTEGER_OUT_OF_RANGE: Code = Code(3001);
/// A mapping without a field the target struct requires. Error.
pub const MISSING_FIELD: Code = Code(3002);
/// An enum variant name the target enum does not have. Error.
pub const UNKNOWN_VARIANT: Code = Code(3003);
/// A custom error raised by the target type's `Deserialize` impl. Error.
pub const CUSTOM: Code = Code(3004);
/// A mapping key the target struct does not accept
/// (`deny_unknown_fields`). Error.
pub const UNKNOWN_FIELD: Code = Code(3005);
/// A value of the right type that the target type rejects. Error.
pub const INVALID_VALUE: Code = Code(3006);
/// A sequence or mapping with a length the target type does not accept,
/// e.g. a tuple of the wrong arity. Error.
pub const INVALID_LENGTH: Code = Code(3007);
/// A struct field given twice (as two keys that differ in YAML but name the
/// same field). Error.
pub const DUPLICATE_FIELD: Code = Code(3008);
/// A single-document entry point (`from_str`, …) given a stream with more
/// than one document; use the multi-document `Deserializer`. Error.
pub const MULTIPLE_DOCUMENTS: Code = Code(3009);
/// Reading the input failed (`from_reader`). Error.
pub const IO_ERROR: Code = Code(3010);
/// A document of a multi-document `Deserializer` used after the stream
/// moved past it, or twice. Error.
pub const DOCUMENT_UNAVAILABLE: Code = Code(3011);
/// A custom error raised by the value's `Serialize` impl
/// (`serde::ser::Error::custom`). Error.
pub const SERIALIZE_CUSTOM: Code = Code(3012);
/// Writing the output failed (`to_writer`, `Serializer`). Error.
pub const WRITE_ERROR: Code = Code(3013);
/// A mapping being serialized has two equal keys (e.g. a struct field and
/// a flattened map entry with the same name); YAML 1.2 requires unique
/// keys, so the output would not read back. Error.
pub const SERIALIZE_DUPLICATE_KEY: Code = Code(3014);
/// A value YAML output cannot represent so that it reads back as the same
/// value: an empty or malformed tag, a tag on a tagged value, or a custom
/// tag on a non-string scalar (a custom-tagged scalar always reads back as
/// a string). Error.
pub const UNREPRESENTABLE_VALUE: Code = Code(3015);
/// A `Serialize` impl drove the serializer out of order, e.g. a map value
/// without its key, or a key without a value. Error.
pub const INVALID_SERIALIZE_CALLS: Code = Code(3016);
/// A `Value` was serialized or deserialized by oneyaml's own `Serializer`
/// or `Deserializer` from a thread-local destructor after oneyaml's own
/// thread-local storage on that thread was destroyed: the value cannot be
/// handed over between the `Value` and oneyaml (an error, never a
/// panic or a wrong or lossy value). Serialize or deserialize before the
/// thread shuts down (or deserialize into a typed target). Error.
pub const THREAD_LOCAL_UNAVAILABLE: Code = Code(3017);
/// A `Deserialize` impl ignored an error that oneyaml returned for a
/// sequence or mapping it had started to read, then went on reading:
/// the rest of that node was never read, so nothing after it can be read
/// in step. Reported at the next read, or at the end of the
/// document, located at the node whose error was ignored; the same for
/// `from_str` and `from_value`. Propagate the error instead (`?`).
/// Error.
pub const READ_AFTER_IGNORED_ERROR: Code = Code(3018);

// ---------------------------------------------------------------------------
// Y9000–Y9999: reserved.
// ---------------------------------------------------------------------------

/// An internal invariant of the library was violated: the
/// release-build outcome of a `debug_assert!`, reported in-band (an error
/// event, or `Err(Internal)`) instead of a silently wrong result. Always a
/// bug in oneyaml; please report it with the input. Error.
pub const INTERNAL_ERROR: Code = Code(9000);
/// A caller-supplied event stream violates the event contract, e.g. one
/// passed to `SyntaxTree::from_events` after filtering or reordering the
/// parser's events. API misuse, not a bug in oneyaml: the result is built
/// without panicking but may not round-trip. Error.
pub const INVALID_EVENT_STREAM: Code = Code(9001);

macro_rules! error_entry {
    ($code:ident, $summary:literal) => {
        CodeInfo {
            code: $code,
            name: stringify!($code),
            severity: Severity::Error,
            summary: $summary,
            deprecated: false,
        }
    };
}

macro_rules! warning_entry {
    ($code:ident, $summary:literal) => {
        CodeInfo {
            code: $code,
            name: stringify!($code),
            severity: Severity::Warning,
            summary: $summary,
            deprecated: false,
        }
    };
}

/// Every allocated code, in numeric order.
pub const REGISTRY: &[CodeInfo] = &[
    error_entry!(
        DIRECTIVE_WITHOUT_DOCUMENT,
        "directives not followed by an explicit document start"
    ),
    error_entry!(MALFORMED_DIRECTIVE, "malformed directive"),
    error_entry!(
        DIRECTIVE_IN_DOCUMENT,
        "directive inside a document not ended with `...`"
    ),
    error_entry!(
        CONTENT_AFTER_DOCUMENT,
        "content after the document's root node or end marker"
    ),
    error_entry!(INVALID_INDENTATION, "content at an invalid indentation"),
    error_entry!(
        BLOCK_COLLECTION_NOT_ALLOWED,
        "block collection cannot start on this line"
    ),
    error_entry!(
        MULTILINE_IMPLICIT_KEY,
        "implicit mapping key spans more than one line"
    ),
    error_entry!(INVALID_NODE_START, "character cannot start a node"),
    error_entry!(UNTERMINATED_QUOTED_SCALAR, "quoted scalar not closed"),
    error_entry!(
        INVALID_ESCAPE,
        "invalid escape sequence in a double-quoted scalar"
    ),
    error_entry!(
        QUOTED_SCALAR_INDENTATION,
        "quoted scalar continuation line not indented enough"
    ),
    error_entry!(INVALID_ANCHOR, "empty or malformed anchor or alias name"),
    error_entry!(INVALID_TAG, "malformed tag"),
    error_entry!(ALIAS_WITH_PROPERTIES, "alias node with an anchor or tag"),
    error_entry!(DUPLICATE_PROPERTY, "node with more than one anchor or tag"),
    error_entry!(TAB_INDENTATION, "tab character used as block indentation"),
    error_entry!(
        UNEXPECTED_CONTENT,
        "unexpected content after a node on the same line"
    ),
    error_entry!(MISSING_MAPPING_COLON, "block mapping entry without `:`"),
    CodeInfo {
        code: UNSUPPORTED_SYNTAX,
        name: "UNSUPPORTED_SYNTAX",
        severity: Severity::Error,
        summary: "syntax not yet supported by this parser version",
        deprecated: true,
    },
    error_entry!(INVALID_BLOCK_HEADER, "malformed block scalar header"),
    error_entry!(
        BLOCK_SCALAR_INDENTATION,
        "block scalar leading empty line indented more than its content"
    ),
    error_entry!(UNTERMINATED_FLOW_COLLECTION, "flow collection not closed"),
    error_entry!(
        MISSING_FLOW_SEPARATOR,
        "flow collection entries not separated by `,`"
    ),
    error_entry!(
        UNEXPECTED_FLOW_INDICATOR,
        "flow indicator where it cannot appear"
    ),
    error_entry!(
        IMPLICIT_KEY_TOO_LONG,
        "implicit key longer than 1024 characters"
    ),
    error_entry!(
        INVALID_QUOTED_CHARACTER,
        "character not allowed in a quoted scalar"
    ),
    error_entry!(
        INLINE_COLON_IN_PLAIN_VALUE,
        "`: ` inside an unquoted block mapping value"
    ),
    warning_entry!(
        LENIENT_INLINE_COLON,
        "`: ` inside an unquoted block mapping value read as text (lenient_inline_colons)"
    ),
    error_entry!(INVALID_UTF8, "invalid UTF-8 in the input"),
    error_entry!(INVALID_UTF16, "invalid UTF-16 in the input"),
    error_entry!(INVALID_UTF32, "invalid UTF-32 in the input"),
    error_entry!(
        MISPLACED_BYTE_ORDER_MARK,
        "byte order mark where no document prefix may start"
    ),
    CodeInfo {
        code: YAML_VERSION_1_1,
        name: "YAML_VERSION_1_1",
        severity: Severity::Warning,
        summary: "%YAML 1.1 directive",
        deprecated: false,
    },
    CodeInfo {
        code: YAML_VERSION_NEWER_MINOR,
        name: "YAML_VERSION_NEWER_MINOR",
        severity: Severity::Warning,
        summary: "%YAML 1.x (x > 2) document processed as YAML 1.2",
        deprecated: false,
    },
    CodeInfo {
        code: YAML_VERSION_UNSUPPORTED_MAJOR,
        name: "YAML_VERSION_UNSUPPORTED_MAJOR",
        severity: Severity::Error,
        summary: "unsupported %YAML major version",
        deprecated: false,
    },
    CodeInfo {
        code: RESERVED_DIRECTIVE_IGNORED,
        name: "RESERVED_DIRECTIVE_IGNORED",
        severity: Severity::Warning,
        summary: "reserved directive ignored",
        deprecated: false,
    },
    error_entry!(
        DUPLICATE_DIRECTIVE,
        "duplicate %YAML directive or %TAG handle"
    ),
    error_entry!(
        UNDEFINED_TAG_HANDLE,
        "tag handle not declared by a %TAG directive"
    ),
    CodeInfo {
        code: YAML_VERSION_1_0,
        name: "YAML_VERSION_1_0",
        severity: Severity::Warning,
        summary: "%YAML 1.0 directive",
        deprecated: false,
    },
    error_entry!(
        INVALID_SCALAR_FOR_TAG,
        "scalar is not a valid form of its tag"
    ),
    error_entry!(COLLECTION_TAG_ON_SCALAR, "collection tag on a scalar"),
    error_entry!(
        WRONG_TAG_ON_COLLECTION,
        "tag does not match the collection kind"
    ),
    error_entry!(
        JSON_UNMATCHED_SCALAR,
        "plain scalar matches no JSON schema type"
    ),
    warning_entry!(
        MEANINGLESS_SETTINGS,
        "setting has no effect in this combination"
    ),
    error_entry!(
        INCORRECT_SETTINGS,
        "settings combination gives incorrect results"
    ),
    error_entry!(DUPLICATE_KEY, "duplicate mapping key"),
    warning_entry!(
        DUPLICATE_KEY_LAST_WINS,
        "duplicate mapping key, last value wins (DuplicateKeys::LastWins)"
    ),
    error_entry!(
        UNDEFINED_ALIAS,
        "alias to an undefined or incomplete anchor"
    ),
    error_entry!(
        INVALID_MERGE_VALUE,
        "merge key value is not a mapping or a sequence of mappings"
    ),
    error_entry!(UNSUPPORTED_SETTING, "setting not supported by this version"),
    error_entry!(
        DEPTH_LIMIT_EXCEEDED,
        "nesting deeper than Limits::max_depth"
    ),
    error_entry!(SCALAR_TOO_LONG, "scalar longer than Limits::max_scalar_len"),
    error_entry!(
        DOCUMENT_TOO_LARGE,
        "input larger than Limits::max_document_size"
    ),
    error_entry!(
        TOO_MANY_DOCUMENTS,
        "more documents than Limits::max_documents"
    ),
    error_entry!(
        ALIAS_BUDGET_EXCEEDED,
        "alias expansion larger than Limits::alias_budget"
    ),
    error_entry!(
        VALUE_DEPTH_EXCEEDED,
        "Value nests deeper than Limits::max_depth"
    ),
    error_entry!(
        TREE_CAPACITY_EXCEEDED,
        "input has more elements than a syntax tree can index"
    ),
    error_entry!(
        TYPED_DEPTH_EXCEEDED,
        "recursive target type nests deeper than Limits::max_typed_depth"
    ),
    error_entry!(
        JSON_DEPTH_EXCEEDED,
        "JSON output nests deeper than 128 levels"
    ),
    error_entry!(
        ALIAS_BYTE_BUDGET_EXCEEDED,
        "alias expansion larger than Limits::alias_byte_budget"
    ),
    warning_entry!(
        DIAGNOSTICS_SUPPRESSED,
        "more diagnostics than Limits::max_diagnostics; the rest are counted, not kept"
    ),
    error_entry!(TYPE_MISMATCH, "node type does not fit the target type"),
    error_entry!(
        INTEGER_OUT_OF_RANGE,
        "integer out of range for the target type"
    ),
    error_entry!(MISSING_FIELD, "missing struct field"),
    error_entry!(UNKNOWN_VARIANT, "unknown enum variant"),
    error_entry!(CUSTOM, "error raised by the target type"),
    error_entry!(UNKNOWN_FIELD, "unknown struct field"),
    error_entry!(INVALID_VALUE, "value rejected by the target type"),
    error_entry!(
        INVALID_LENGTH,
        "collection length rejected by the target type"
    ),
    error_entry!(DUPLICATE_FIELD, "struct field given twice"),
    error_entry!(
        MULTIPLE_DOCUMENTS,
        "more than one document for a single-document entry point"
    ),
    error_entry!(IO_ERROR, "I/O error while reading the input"),
    error_entry!(
        DOCUMENT_UNAVAILABLE,
        "document no longer available from the multi-document deserializer"
    ),
    error_entry!(
        SERIALIZE_CUSTOM,
        "error raised by the value's Serialize impl"
    ),
    error_entry!(WRITE_ERROR, "I/O error while writing the output"),
    error_entry!(
        SERIALIZE_DUPLICATE_KEY,
        "serialized mapping has two equal keys"
    ),
    error_entry!(
        UNREPRESENTABLE_VALUE,
        "value cannot be represented in YAML output"
    ),
    error_entry!(
        INVALID_SERIALIZE_CALLS,
        "Serialize impl drove the serializer out of order"
    ),
    error_entry!(
        THREAD_LOCAL_UNAVAILABLE,
        "Value serialized or deserialized during thread-local storage teardown"
    ),
    error_entry!(
        READ_AFTER_IGNORED_ERROR,
        "deserializer used after one of its errors was ignored"
    ),
    error_entry!(INTERNAL_ERROR, "internal error (a bug in oneyaml)"),
    error_entry!(
        INVALID_EVENT_STREAM,
        "caller-supplied event stream violates the event contract"
    ),
];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    const RANGES: [CodeRange; 5] = [
        CodeRange::Syntax,
        CodeRange::Schema,
        CodeRange::Limits,
        CodeRange::Serde,
        CodeRange::Reserved,
    ];

    #[test]
    fn registry_is_sequential_within_each_range() {
        for range in RANGES {
            let (lo, _) = range.bounds();
            let codes: Vec<u16> = REGISTRY
                .iter()
                .filter(|i| i.code.range() == range)
                .map(|i| i.code.get())
                .collect();
            let expected: Vec<u16> = (0..codes.len())
                .map(|k| lo.max(1).checked_add(u16::try_from(k).unwrap()).unwrap())
                .collect();
            // Retired codes are flagged, never removed, so every range stays
            // contiguous: a new code is always max + 1.
            assert_eq!(codes, expected, "{range:?} codes are contiguous");
        }
    }

    #[test]
    fn registry_is_sorted_unique_and_named() {
        for pair in REGISTRY.windows(2) {
            assert!(pair[0].code < pair[1].code);
        }
        for info in REGISTRY {
            assert_eq!(info.code.info(), Some(info));
            assert!(!info.summary.is_empty());
            assert!(!info.name.is_empty());
        }
    }

    #[test]
    fn range_validation() {
        assert_eq!(Code::new(0), None);
        assert_eq!(Code::new(1).unwrap().range(), CodeRange::Syntax);
        assert_eq!(Code::new(999).unwrap().range(), CodeRange::Syntax);
        assert_eq!(Code::new(1000).unwrap().range(), CodeRange::Schema);
        assert_eq!(Code::new(2500).unwrap().range(), CodeRange::Limits);
        assert_eq!(Code::new(3999).unwrap().range(), CodeRange::Serde);
        assert_eq!(Code::new(4000), None);
        assert_eq!(Code::new(8999), None);
        assert_eq!(Code::new(9000).unwrap().range(), CodeRange::Reserved);
        assert_eq!(Code::new(10000), None);
        for range in RANGES {
            let (lo, hi) = range.bounds();
            assert!(lo <= hi);
            assert_eq!(Code::new(hi).unwrap().range(), range);
        }
    }

    #[test]
    fn display_and_parse_round_trip() {
        assert_eq!(YAML_VERSION_1_1.to_string(), "Y1000");
        assert_eq!(Code::new(7).unwrap().to_string(), "Y0007");
        assert_eq!(Code::parse("Y1003"), Some(RESERVED_DIRECTIVE_IGNORED));
        assert_eq!(Code::parse("Y0007"), Code::new(7));
        for bad in [
            "", "Y", "Y0000", "Y123", "Y12345", "X1000", "Y10a0", "Y5000",
        ] {
            assert_eq!(Code::parse(bad), None, "{bad}");
        }
        assert_eq!(Code::new(999).unwrap().info(), None);
    }
}
