//! Every active error code in the registry has a negative test
//! that **runs** and produces it. This crate sees every layer (decoder,
//! parser, tree builder, resolver, construction, serde), so the check lives
//! here.
//!
//! How "somewhere" is established so it cannot rot silently:
//!
//! * [`drivers`] maps each code to a closure that drives the public API and
//!   returns the codes it produced; the check asserts the code is among
//!   them. A driver that stops producing its code fails the test.
//! * A code no input can reach through the public API (an internal
//!   invariant, a limit needing more than 4 Gi elements, a check whose
//!   consumer does not exist yet) is listed in [`EXEMPT`] with the unit
//!   test that drives it directly. The check asserts that test function
//!   exists in that file and names the code's constant in its body.
//! * Every active error code must be in exactly one of the two lists, so a
//!   new code without a negative test fails the test, and so does a stale
//!   entry for a code that was deprecated.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeSet;
use std::path::Path;

use oneyaml::code::{self, REGISTRY};
use oneyaml::{
    Code, Deserializer, Emit, Limits, Schema, Settings, Severity, Tag, TaggedValue, Value,
    from_reader, from_str, from_str_with,
};
use oneyaml_syntax::SyntaxTree;
use serde::Deserialize;
use serde::de::Error as _;

/// Codes that no input reaches through the public API, each with the unit
/// test that drives it: `(code, file under crates/, test function)`.
const EXEMPT: &[(Code, &str, &str)] = &[
    // Needs more than 2^32 tree elements; the test lowers the cap.
    (
        code::TREE_CAPACITY_EXCEEDED,
        "oneyaml-syntax/src/tree/tests.rs",
        "a_full_arena_skips_the_rest",
    ),
    // Behind the optional `serde_json` feature (on in the workspace through
    // the bench crate's dev-dependency): its integration test drives it.
    (
        code::JSON_DEPTH_EXCEEDED,
        "oneyaml/tests/json_convert.rs",
        "json_output_depth_is_bounded",
    ),
    // Internal invariants: no input breaks them.
    (
        code::INTERNAL_ERROR,
        "oneyaml-syntax/src/tree/tests.rs",
        "contract_violations_are_coded_by_origin",
    ),
];

/// What a driver reports: the codes it produced, or a declared platform
/// skip (never a made-up code). The check accepts a skip only for a code
/// listed in [`PLATFORM_DEPENDENT`] on a platform that entry declares
/// unsupported.
#[derive(Debug)]
enum Outcome {
    Codes(Vec<Code>),
    PlatformSkip(String),
}

impl FromIterator<Code> for Outcome {
    fn from_iter<I: IntoIterator<Item = Code>>(iter: I) -> Self {
        Self::Codes(iter.into_iter().collect())
    }
}

type Driver = Box<dyn Fn() -> Outcome>;

/// Codes whose driver needs a platform guarantee: `(code, supported on
/// this platform)`. Where supported, the driver must produce the code;
/// elsewhere it may report [`Outcome::PlatformSkip`], and the check says
/// so instead of counting the code as covered.
const PLATFORM_DEPENDENT: &[(Code, bool)] = &[(code::THREAD_LOCAL_UNAVAILABLE, TLS_DTORS_LIFO)];

/// Every diagnostic code of the parser's events for `src` under `settings`.
fn events(src: &'static str, settings: Settings) -> Driver {
    Box::new(move || {
        oneyaml_syntax::parse_with(src, &settings)
            .iter()
            .filter_map(|e| e.diagnostic().map(|d| d.code))
            .collect()
    })
}

fn owned_events(src: String) -> Driver {
    Box::new(move || {
        oneyaml_syntax::parse(&src)
            .iter()
            .filter_map(|e| e.diagnostic().map(|d| d.code))
            .collect()
    })
}

fn decoded(bytes: &'static [u8]) -> Driver {
    Box::new(move || {
        oneyaml_syntax::decode(bytes)
            .diagnostics()
            .iter()
            .map(|d| d.code)
            .collect()
    })
}

/// The error code of `from_str_with::<T>`.
fn typed<T: for<'de> Deserialize<'de>>(src: &'static str, settings: Settings) -> Driver {
    Box::new(move || {
        from_str_with::<T>(src, &settings)
            .err()
            .map(|e| e.code())
            .into_iter()
            .collect()
    })
}

/// Reads a sequence; ignores the error of reading its first item as a
/// `u8`, then reads on: `Y3018` when that item could not be skipped (it
/// holds an input error).
struct IgnoresAnError;

impl<'de> Deserialize<'de> for IgnoresAnError {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = IgnoresAnError;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a sequence")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<IgnoresAnError, A::Error> {
                let _ = a.next_element::<u8>();
                while a.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(IgnoresAnError)
            }
        }
        d.deserialize_seq(V)
    }
}

fn value(src: &'static str) -> Driver {
    typed::<Value>(src, Settings::new())
}

fn limited(src: &'static str, limits: Limits) -> Driver {
    events(src, Settings::new().limits(limits))
}

fn nested(depth: usize) -> String {
    format!("{}{}", "[".repeat(depth), "]".repeat(depth))
}

#[derive(Debug, Deserialize)]
struct Tree(#[allow(dead_code)] Vec<Tree>);

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Need {
    a: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct Strict {
    a: u8,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Aliased {
    #[serde(alias = "b")]
    a: u8,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
enum Shape {
    Unit,
}

/// A target whose `Deserialize` always fails with its own message.
#[derive(Debug)]
struct Refuses;

impl<'de> Deserialize<'de> for Refuses {
    fn deserialize<D: serde::Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
        Err(D::Error::custom("refused"))
    }
}

struct Broken;

impl std::io::Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("broken"))
    }
}

/// The error code of serializing `v` with `settings`.
fn serialized(v: Value, settings: Settings) -> Driver {
    Box::new(move || {
        oneyaml::to_string_with(&v, &settings)
            .err()
            .map(|e| e.code())
            .into_iter()
            .collect()
    })
}

/// A value whose `Serialize` always fails with its own message.
struct Unserializable;

impl serde::Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("refused"))
    }
}

/// A `Serialize` impl that writes a map key without its value.
struct KeyWithoutValue;

impl serde::Serialize for KeyWithoutValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        m.serialize_key("k")?;
        m.end()
    }
}

#[derive(serde::Serialize)]
struct Flat {
    a: i32,
    #[serde(flatten)]
    rest: std::collections::BTreeMap<String, i32>,
}

struct BrokenWriter;

impl std::io::Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("broken"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn drivers() -> Vec<(Code, Driver)> {
    let tiny = Limits::new;
    vec![
        // Y0xxx: decode, lex, parse, recovery.
        (
            code::DIRECTIVE_WITHOUT_DOCUMENT,
            events("%YAML 1.2\n", Settings::new()),
        ),
        (
            code::MALFORMED_DIRECTIVE,
            events("%YAML\n---\n", Settings::new()),
        ),
        (
            code::DUPLICATE_YAML_DIRECTIVE,
            events("%YAML 1.2\n%YAML 1.2\n---\n", Settings::new()),
        ),
        (
            code::INVALID_COMMENT_CHARACTER,
            events("a: 1 # c\u{7f}\n", Settings::new()),
        ),
        (
            code::DIRECTIVE_IN_DOCUMENT,
            events("---\n%YAML 1.2\n---\n", Settings::new()),
        ),
        (
            code::CONTENT_AFTER_DOCUMENT,
            events("'a'\nb\n", Settings::new()),
        ),
        (
            code::INVALID_INDENTATION,
            events("a:\n  b: c\n d: e\n", Settings::new()),
        ),
        (
            code::BLOCK_COLLECTION_NOT_ALLOWED,
            events("--- a: b\n", Settings::new()),
        ),
        (
            code::MULTILINE_IMPLICIT_KEY,
            events("a\nb: c\n", Settings::new()),
        ),
        (code::INVALID_NODE_START, events("@a\n", Settings::new())),
        (
            code::UNTERMINATED_QUOTED_SCALAR,
            events("'a\n", Settings::new()),
        ),
        (code::INVALID_ESCAPE, events("\"\\q\"\n", Settings::new())),
        (
            code::QUOTED_SCALAR_INDENTATION,
            events("k: \"a\nb\"\n", Settings::new()),
        ),
        (code::INVALID_ANCHOR, events("& a\n", Settings::new())),
        (code::INVALID_TAG, events("!!\n", Settings::new())),
        (
            code::ALIAS_WITH_PROPERTIES,
            events("&a *b\n", Settings::new()),
        ),
        (
            code::DUPLICATE_PROPERTY,
            events("&a &b c\n", Settings::new()),
        ),
        (
            code::TAB_INDENTATION,
            events("a:\n\tb: c\n", Settings::new()),
        ),
        (code::UNEXPECTED_CONTENT, events("'a' b\n", Settings::new())),
        (
            code::MISSING_MAPPING_COLON,
            events("a: 1\nb\n", Settings::new()),
        ),
        (
            code::INVALID_BLOCK_HEADER,
            events("a: |0\n  x\n", Settings::new()),
        ),
        (
            code::BLOCK_SCALAR_INDENTATION,
            events("a: |\n   \n  x\n", Settings::new()),
        ),
        (
            code::UNTERMINATED_FLOW_COLLECTION,
            events("[a, b\n", Settings::new()),
        ),
        (
            code::MISSING_FLOW_SEPARATOR,
            events("[a\n b\n# c\n c]\n", Settings::new()),
        ),
        (
            code::UNEXPECTED_FLOW_INDICATOR,
            events("[, a]\n", Settings::new()),
        ),
        (
            code::IMPLICIT_KEY_TOO_LONG,
            owned_events(format!("{}: v\n", "k".repeat(1025))),
        ),
        (
            code::INVALID_QUOTED_CHARACTER,
            events("\"a\u{1}\"\n", Settings::new()),
        ),
        (
            code::INLINE_COLON_IN_PLAIN_VALUE,
            events("a: b: c\n", Settings::new()),
        ),
        (code::INVALID_UTF8, decoded(b"a\xFF")),
        (code::INVALID_UTF16, decoded(b"a\x00\x00\xD8")),
        (
            code::INVALID_UTF32,
            decoded(b"a\x00\x00\x00\x00\xD8\x00\x00"),
        ),
        (
            code::MISPLACED_BYTE_ORDER_MARK,
            events("a: 1\n  \u{feff}b: 2\n", Settings::new()),
        ),
        // Y1xxx: schema & resolution.
        (
            code::YAML_VERSION_UNSUPPORTED_MAJOR,
            events("%YAML 2.0\n---\na\n", Settings::new()),
        ),
        (
            code::DUPLICATE_DIRECTIVE,
            events("%TAG !a! x\n%TAG !a! y\n---\n", Settings::new()),
        ),
        (
            code::UNDEFINED_TAG_HANDLE,
            events("!e!x a\n", Settings::new()),
        ),
        (code::INVALID_SCALAR_FOR_TAG, value("!!int twelve")),
        (code::COLLECTION_TAG_ON_SCALAR, value("!!seq a")),
        (code::WRONG_TAG_ON_COLLECTION, value("!!str [a]")),
        (
            code::JSON_UNMATCHED_SCALAR,
            typed::<Value>("[1, hello]", Settings::new().schema(Schema::Json)),
        ),
        (code::DUPLICATE_KEY, value("a: 1\na: 2\n")),
        (code::UNDEFINED_ALIAS, value("a: *nope\n")),
        (
            code::INVALID_MERGE_VALUE,
            typed::<Value>("a: {<<: 1}\n", Settings::new().merge_keys(true)),
        ),
        // Y2xxx: limits.
        (
            code::DEPTH_LIMIT_EXCEEDED,
            limited("- - a\n", tiny().max_depth(1)),
        ),
        (
            code::SCALAR_TOO_LONG,
            limited("abc\n", tiny().max_scalar_len(2)),
        ),
        (
            code::DOCUMENT_TOO_LARGE,
            limited("1234567890", tiny().max_document_size(9)),
        ),
        (
            code::TOO_MANY_DOCUMENTS,
            limited("a\n--- b\n", tiny().max_documents(1)),
        ),
        (
            code::ALIAS_BUDGET_EXCEEDED,
            typed::<Value>(
                "- &a [1, 2]\n- *a\n- *a\n",
                Settings::new().limits(tiny().alias_budget(2)),
            ),
        ),
        (
            code::ALIAS_BYTE_BUDGET_EXCEEDED,
            typed::<Value>(
                "- &a abcdef\n- *a\n- *a\n",
                Settings::new().limits(tiny().alias_byte_budget(10)),
            ),
        ),
        (
            code::TYPED_DEPTH_EXCEEDED,
            Box::new(|| {
                let src = nested(129);
                from_str::<Tree>(&src)
                    .err()
                    .map(|e| e.code())
                    .into_iter()
                    .collect()
            }),
        ),
        // Y3xxx: serde.
        (code::TYPE_MISMATCH, typed::<u8>("[a]", Settings::new())),
        (
            code::INTEGER_OUT_OF_RANGE,
            typed::<u8>("300", Settings::new()),
        ),
        (code::MISSING_FIELD, typed::<Need>("b: 1", Settings::new())),
        (
            code::UNKNOWN_VARIANT,
            typed::<Shape>("Round", Settings::new()),
        ),
        (code::CUSTOM, typed::<Refuses>("1", Settings::new())),
        (
            code::UNKNOWN_FIELD,
            typed::<Strict>("a: 1\nb: 2", Settings::new()),
        ),
        (code::INVALID_VALUE, typed::<char>("xy", Settings::new())),
        (
            code::INVALID_LENGTH,
            typed::<(u8, u8)>("[1, 2, 3]", Settings::new()),
        ),
        (
            code::DUPLICATE_FIELD,
            typed::<Aliased>("{a: 1, b: 2}", Settings::new()),
        ),
        (code::MULTIPLE_DOCUMENTS, value("a\n---\nb\n")),
        (
            code::IO_ERROR,
            Box::new(|| {
                from_reader::<_, Value>(Broken)
                    .err()
                    .map(|e| e.code())
                    .into_iter()
                    .collect()
            }),
        ),
        (
            code::DOCUMENT_UNAVAILABLE,
            Box::new(|| {
                let mut it = Deserializer::from_str("1\n--- 2\n");
                let first = it.next().unwrap();
                let _second = it.next().unwrap();
                Value::deserialize(first)
                    .err()
                    .map(|e| e.code())
                    .into_iter()
                    .collect()
            }),
        ),
        (
            // A visitor ignores an error for an item that
            // cannot be skipped, and reads on.
            code::READ_AFTER_IGNORED_ERROR,
            typed::<IgnoresAnError>("[{a: 1, a: 2}, 2]", Settings::new()),
        ),
        // Serializer: settings, limits, and values it refuses.
        (
            code::INCORRECT_SETTINGS,
            serialized(Value::from(1), Settings::new().indent(0)),
        ),
        (
            code::UNSUPPORTED_SETTING,
            serialized(
                Value::from(1),
                Settings::new().emit(Emit::new().line_width(Some(80))),
            ),
        ),
        (
            code::VALUE_DEPTH_EXCEEDED,
            serialized(
                Value::from(vec![Value::from(vec![Value::Null])]),
                Settings::new().max_depth(1),
            ),
        ),
        (
            code::SERIALIZE_CUSTOM,
            Box::new(|| {
                oneyaml::to_string(&Unserializable)
                    .err()
                    .map(|e| e.code())
                    .into_iter()
                    .collect()
            }),
        ),
        (
            code::WRITE_ERROR,
            Box::new(|| {
                oneyaml::to_writer(BrokenWriter, &1)
                    .err()
                    .map(|e| e.code())
                    .into_iter()
                    .collect()
            }),
        ),
        (
            code::SERIALIZE_DUPLICATE_KEY,
            Box::new(|| {
                let v = Flat {
                    a: 1,
                    rest: std::collections::BTreeMap::from([("a".to_owned(), 2)]),
                };
                oneyaml::to_string(&v)
                    .err()
                    .map(|e| e.code())
                    .into_iter()
                    .collect()
            }),
        ),
        (
            code::UNREPRESENTABLE_VALUE,
            serialized(
                Value::from(TaggedValue {
                    tag: Tag::new("!n"),
                    value: Value::from(1),
                }),
                Settings::new(),
            ),
        ),
        (
            code::INVALID_SERIALIZE_CALLS,
            Box::new(|| {
                oneyaml::to_string(&KeyWithoutValue)
                    .err()
                    .map(|e| e.code())
                    .into_iter()
                    .collect()
            }),
        ),
        (
            code::THREAD_LOCAL_UNAVAILABLE,
            Box::new(value_in_tls_destructor),
        ),
        // Y9xxx.
        (
            code::INVALID_EVENT_STREAM,
            Box::new(|| {
                let src = "a: [b]\n";
                let events = oneyaml_syntax::parse(src);
                SyntaxTree::from_events(events.sourced().skip(2))
                    .diagnostics()
                    .map(|d| d.code)
                    .collect()
            }),
        ),
    ]
}

/// Whether `outcome` covers `c`: `Ok(None)` when it produced `c`,
/// `Ok(Some(reason))` for a skip that `platform` declares (the code is
/// then reported, not counted), `Err` otherwise.
fn check_outcome(
    c: Code,
    outcome: Outcome,
    platform: &[(Code, bool)],
) -> Result<Option<String>, String> {
    match outcome {
        Outcome::Codes(got) if got.contains(&c) => Ok(None),
        Outcome::Codes(got) => Err(format!("driver for {c} produced {got:?}")),
        Outcome::PlatformSkip(reason) => {
            let supported = platform.iter().find(|(p, _)| *p == c).map(|(_, s)| *s);
            if supported == Some(false) {
                Ok(Some(format!("{c} not covered on this platform: {reason}")))
            } else {
                Err(format!(
                    "driver for {c} skipped ({reason}), but {c} is not declared \
                     unsupported on this platform in PLATFORM_DEPENDENT"
                ))
            }
        }
    }
}

/// The check accepts a skip only where it is declared, and never counts it
/// as the code.
#[test]
fn platform_skips_are_declared_never_fabricated() {
    let tls = code::THREAD_LOCAL_UNAVAILABLE;
    let skip = || Outcome::PlatformSkip("no LIFO".to_owned());
    // Declared unsupported: reported as a skip.
    let got = check_outcome(tls, skip(), &[(tls, false)]).unwrap();
    assert!(got.unwrap().contains("not covered"));
    // Declared supported, or not declared at all: a failure.
    assert!(check_outcome(tls, skip(), &[(tls, true)]).is_err());
    assert!(check_outcome(tls, skip(), &[]).is_err());
    assert!(check_outcome(code::CUSTOM, skip(), &[(tls, false)]).is_err());
    // A partial or empty result never covers the code.
    assert!(check_outcome(tls, Outcome::Codes(vec![]), &[(tls, false)]).is_err());
    assert_eq!(check_outcome(tls, Outcome::Codes(vec![tls]), &[]), Ok(None));
    // On this platform, the real table agrees with the driver's guard.
    assert_eq!(
        PLATFORM_DEPENDENT.iter().find(|(c, _)| *c == tls),
        Some(&(tls, TLS_DTORS_LIFO))
    );
}

/// The body of `fn name` in `text`: up to the next `fn ` at the start of a
/// line (tests are top-level functions, or in a module indented once).
fn fn_body<'t>(text: &'t str, name: &str) -> Option<&'t str> {
    let start = text.find(&format!("fn {name}("))?;
    let rest = text.get(start..)?;
    let end = rest
        .get(1..)
        .and_then(|r| r.find("\nfn ").or_else(|| r.find("\n    fn ")))
        .map_or(rest.len(), |e| e.saturating_add(1));
    rest.get(..end)
}

/// `fn name` carries `#[test]` among the attributes and docs right above.
fn is_test(text: &str, name: &str) -> bool {
    let Some(at) = text.find(&format!("fn {name}(")) else {
        return false;
    };
    let before = text.get(..at).unwrap_or("");
    // Up to the start of the `fn` line.
    let before = before.get(..before.rfind('\n').unwrap_or(0)).unwrap_or("");
    before
        .lines()
        .rev()
        .map(str::trim)
        .take_while(|l| l.starts_with("#[") || l.starts_with("//"))
        .any(|l| l == "#[test]")
}

#[test]
fn every_active_error_code_has_a_negative_test() {
    let active: BTreeSet<Code> = REGISTRY
        .iter()
        .filter(|i| i.severity == Severity::Error && !i.deprecated)
        .map(|i| i.code)
        .collect();
    let drivers = drivers();
    let driven: BTreeSet<Code> = drivers.iter().map(|(c, _)| *c).collect();
    assert_eq!(driven.len(), drivers.len(), "a code is driven twice");
    let exempt: BTreeSet<Code> = EXEMPT.iter().map(|(c, _, _)| *c).collect();
    assert_eq!(exempt.len(), EXEMPT.len(), "a code is exempted twice");

    for c in &active {
        assert!(
            driven.contains(c) != exempt.contains(c),
            "{c} ({}) needs exactly one negative test: a driver here, or an EXEMPT \
             entry naming the unit test that reaches it",
            c.info().map_or("?", |i| i.name)
        );
    }
    for c in driven.iter().chain(&exempt) {
        assert!(active.contains(c), "{c} is not an active error code");
    }

    // Each driver really produces its code, or reports a declared skip.
    for (c, drive) in &drivers {
        let checked = check_outcome(*c, drive(), PLATFORM_DEPENDENT);
        if let Some(skip) = checked.unwrap_or_else(|e| panic!("{e}")) {
            eprintln!("skipped: {skip}");
        }
    }

    // Each exemption names a test that exists and names the code. The
    // tests live across the workspace's crates, which a published package
    // does not have beside it: from the unpacked crate this part is
    // skipped, saying so.
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    if !crates.join("oneyaml-core").is_dir() || !crates.join("oneyaml-syntax").is_dir() {
        // Inside the workspace the sibling crates must be there (a skip
        // there would make this check a silent no-op).
        let manifest = crates.join("../Cargo.toml");
        let workspace = std::fs::read_to_string(manifest)
            .is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""));
        assert!(!workspace, "sibling crates missing inside the workspace");
        eprintln!(
            "skipped: exemption check needs the oneyaml workspace (sibling crates not found at {})",
            crates.display()
        );
        return;
    }
    for (c, file, test) in EXEMPT {
        let text = std::fs::read_to_string(crates.join(file)).unwrap();
        let body = fn_body(&text, test)
            .unwrap_or_else(|| panic!("{file} has no fn {test} (exemption for {c})"));
        let name = c.info().unwrap().name;
        assert!(
            body.contains(name),
            "{file}::{test} must name {name} to cover {c}"
        );
        assert!(is_test(&text, test), "{file}::{test} is not a #[test]");
    }
}

/// Codes seen by [`DeserializeOnDrop`].
static TLS_CODES: std::sync::Mutex<Vec<Code>> = std::sync::Mutex::new(Vec::new());

/// Deserializes and serializes a `Value` while the thread's thread-locals
/// are destroyed.
struct DeserializeOnDrop;

impl Drop for DeserializeOnDrop {
    fn drop(&mut self) {
        let de = from_str::<Value>("[1]").err();
        let ser = oneyaml::to_string(&Value::from(vec![Value::from(1)])).err();
        let mut codes = TLS_CODES.lock().unwrap();
        codes.extend(de.into_iter().chain(ser).map(|e| e.code()));
    }
}

thread_local! {
    static ON_DROP: DeserializeOnDrop = const { DeserializeOnDrop };
}

/// Whether thread-local destructors run in reverse order of registration
/// (see `ser_regressions.rs`, `TLS_DTORS_LIFO`): Linux, Android and Windows.
/// Rust's std documents no order, so elsewhere the driver is skipped.
const TLS_DTORS_LIFO: bool = cfg!(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
));

/// `Y3017`: registered before oneyaml's hand-off slots, the destructor
/// runs after those slots are gone (on the platforms of `TLS_DTORS_LIFO`,
/// thread-local destructors run in reverse order of registration). Both
/// the deserialize and the serialize hand-over refuse with `Y3017`.
fn value_in_tls_destructor() -> Outcome {
    std::thread::spawn(|| {
        ON_DROP.with(|_| ());
        let _ = from_str::<Value>("1");
        let _ = oneyaml::to_string(&Value::from(1));
    })
    .join()
    .unwrap();
    let codes = std::mem::take(&mut *TLS_CODES.lock().unwrap());
    let both = [
        code::THREAD_LOCAL_UNAVAILABLE,
        code::THREAD_LOCAL_UNAVAILABLE,
    ];
    if TLS_DTORS_LIFO {
        assert_eq!(codes, both, "deserialize, then serialize");
    } else if codes != both {
        // No destructor order guarantee here: the destructor may have run
        // while some of oneyaml's slots were still alive, so neither an
        // empty nor a partial result covers both hand-overs. A declared
        // skip, never a fabricated code.
        return Outcome::PlatformSkip(format!(
            "needs LIFO thread-local destructors (got {codes:?})"
        ));
    }
    Outcome::Codes(codes)
}
