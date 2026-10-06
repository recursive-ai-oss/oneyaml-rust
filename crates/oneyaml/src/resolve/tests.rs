#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unreachable
)]
//! Schema tables built from YAML 1.2.2 §10.1–10.3 (examples and regexes)
//! and yaml.org/type for the 1.1 settings.

use oneyaml_core::{Compat, Emit, Schema, Settings};

use super::conflict::{Conflict, ConflictSeverity, check_settings};
use super::*;

/// Expected outcome.
#[derive(Debug, Clone, Copy)]
enum X {
    N,
    B(bool),
    I(i128),
    U(u128),
    F(f64),
    Nan,
    S,
    /// A wide integer, by its `Display`.
    Big(&'static str),
    Custom(&'static str),
    E(ResolveError),
}
use X::*;

fn check(got: Result<Resolved<'_>, ResolveError>, want: X, ctx: &str) {
    let ok = match (&got, want) {
        (Ok(Resolved::Null), N) | (Ok(Resolved::Str), S) => true,
        (Ok(Resolved::Bool(a)), B(b)) => *a == b,
        (Ok(Resolved::Number(n)), I(i)) => n.is_integer() && n.as_i128() == Some(i),
        (Ok(Resolved::Number(n)), U(u)) => n.is_integer() && n.as_u128() == Some(u),
        (Ok(Resolved::Number(n)), F(f)) => n.as_float().map(f64::to_bits) == Some(f.to_bits()),
        (Ok(Resolved::Number(n)), Nan) => n.is_nan(),
        (Ok(Resolved::Number(n)), Big(s)) => n.is_big_integer() && n.to_string() == s,
        (Ok(Resolved::Custom(t)), Custom(u)) => *t == u,
        (Err(e), E(f)) => *e == f,
        _ => false,
    };
    assert!(ok, "{ctx}: got {got:?}, want {want:?}");
}

fn plain_table(settings: &Settings, table: &[(&str, X)]) {
    let r = Resolver::new(settings);
    for (text, want) in table {
        let got = r.resolve_scalar(TagInput::Absent, Style::Plain, text);
        check(
            got,
            *want,
            &format!("{:?} plain {text:?}", settings.get_schema()),
        );
    }
}

const INT: &str = "tag:yaml.org,2002:int";
const FLOAT: &str = "tag:yaml.org,2002:float";
const BOOL: &str = "tag:yaml.org,2002:bool";
const NULL: &str = "tag:yaml.org,2002:null";
const STR: &str = "tag:yaml.org,2002:str";
const SEQ: &str = "tag:yaml.org,2002:seq";
const MAP: &str = "tag:yaml.org,2002:map";

/// Examples shared by Core with and without extensions where 1.1 must not
/// change the answer.
const CORE: &[(&str, X)] = &[
    // §10.3.2 null
    ("", N),
    ("~", N),
    ("null", N),
    ("Null", N),
    ("NULL", N),
    ("nULL", S),
    ("NuLL", S),
    ("none", S),
    // bool
    ("true", B(true)),
    ("True", B(true)),
    ("TRUE", B(true)),
    ("false", B(false)),
    ("False", B(false)),
    ("FALSE", B(false)),
    ("tRUE", S),
    // int, base 10: [-+]? [0-9]+
    ("0", I(0)),
    ("12", I(12)),
    ("+12", I(12)),
    ("-12", I(-12)),
    ("-0", I(0)),
    ("007", I(7)),
    ("-", S),
    ("+", S),
    ("+-1", S),
    ("1-", S),
    // int, base 8: 0o [0-7]+
    ("0o7", I(7)),
    ("0o17", I(15)),
    ("0o777", I(511)),
    ("0o", S),
    ("0o8", S),
    ("-0o17", S),
    ("+0o17", S),
    ("0O17", S),
    // int, base 16: 0x [0-9a-fA-F]+
    ("0xFF", I(255)),
    ("0xff", I(255)),
    ("0x1f", I(31)),
    ("0x", S),
    ("0x1g", S),
    ("-0x10", S),
    ("0XFF", S),
    // float
    ("1e3", F(1000.0)),
    ("1.0e3", F(1000.0)),
    ("1E3", F(1000.0)),
    ("1e+3", F(1000.0)),
    ("1e-3", F(0.001)),
    ("1.", F(1.0)),
    ("1.5", F(1.5)),
    ("-1.5", F(-1.5)),
    ("+1.5", F(1.5)),
    (".5", F(0.5)),
    ("+.5", F(0.5)),
    ("-.5", F(-0.5)),
    ("-0.0", F(-0.0)),
    ("0.", F(0.0)),
    ("1e400", F(f64::INFINITY)),
    (".", S),
    (".e3", S),
    ("1e", S),
    ("1e+", S),
    ("1.5e3.0", S),
    ("1.2.3", S),
    ("e3", S),
    (".inf", F(f64::INFINITY)),
    (".Inf", F(f64::INFINITY)),
    (".INF", F(f64::INFINITY)),
    ("+.inf", F(f64::INFINITY)),
    ("-.inf", F(f64::NEG_INFINITY)),
    ("-.Inf", F(f64::NEG_INFINITY)),
    (".iNF", S),
    ("inf", S),
    ("infinity", S),
    (".nan", Nan),
    (".NaN", Nan),
    (".NAN", Nan),
    ("-.nan", S),
    ("+.nan", S),
    (".nAn", S),
    ("nan", S),
    // Everything else: str.
    ("hello", S),
    ("0b101", S),
    ("12 ", S),
    (" 12", S),
];

/// Core-only answers that 1.1 settings change.
const CORE_NO_EXT: &[(&str, X)] = &[
    ("0777", I(777)),
    ("y", S),
    ("Y", S),
    ("yes", S),
    ("Yes", S),
    ("no", S),
    ("n", S),
    ("on", S),
    ("off", S),
    ("OFF", S),
    ("1_000", S),
    ("1_000.5", S),
    ("12:30:00", S),
    ("1:30.5", S),
    ("0x_ff", S),
];

#[test]
fn core_schema() {
    plain_table(&Settings::new(), CORE);
    plain_table(&Settings::new(), CORE_NO_EXT);
    assert_eq!(Settings::default().get_schema(), Schema::Core);
}

#[test]
fn core_integer_width() {
    plain_table(
        &Settings::new(),
        &[
            ("170141183460469231731687303715884105727", I(i128::MAX)),
            ("-170141183460469231731687303715884105728", I(i128::MIN)),
            ("340282366920938463463374607431768211455", U(u128::MAX)),
            (
                "340282366920938463463374607431768211456",
                Big("340282366920938463463374607431768211456"),
            ),
            (
                "-000340282366920938463463374607431768211456",
                Big("-340282366920938463463374607431768211456"),
            ),
            (
                "0x100000000000000000000000000000000",
                Big("0x100000000000000000000000000000000"),
            ),
            (
                "0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
                Big("0xfffffffffffffffffffffffffffffffff"),
            ),
            ("0xffffffffffffffffffffffffffffffff", U(u128::MAX)),
            (
                "0o7777777777777777777777777777777777777777777",
                Big("0o7777777777777777777777777777777777777777777"),
            ),
        ],
    );
    // A negative magnitude up to u128::MAX is exact but not an i128.
    let r = Resolver::new(&Settings::new());
    let Ok(Resolved::Number(n)) = r.resolve_scalar(
        TagInput::Absent,
        Style::Plain,
        "-340282366920938463463374607431768211455",
    ) else {
        unreachable!()
    };
    assert!(!n.is_big_integer() && n.as_i128().is_none() && n.as_u128().is_none());
    assert_eq!(n.to_string(), "-340282366920938463463374607431768211455");
    // Wide integers compare by radix and digits.
    let big = |t| match r.resolve_scalar(TagInput::Absent, Style::Plain, t) {
        Ok(Resolved::Number(n)) => n,
        other => unreachable!("{other:?}"),
    };
    assert_eq!(
        big("0x100000000000000000000000000000000"),
        big("0x000100000000000000000000000000000000")
    );
    assert_ne!(
        big("340282366920938463463374607431768211456"),
        big("340282366920938463463374607431768211457")
    );
    assert_ne!(
        big("340282366920938463463374607431768211456"),
        big("-340282366920938463463374607431768211456")
    );
}

const JSON: &[(&str, X)] = &[
    // §10.2.2
    ("null", N),
    ("true", B(true)),
    ("false", B(false)),
    ("0", I(0)),
    ("-0", I(0)),
    ("12", I(12)),
    ("-12", I(-12)),
    ("1.5", F(1.5)),
    ("-1.5", F(-1.5)),
    ("1.", F(1.0)),
    ("0.5", F(0.5)),
    ("1e3", F(1000.0)),
    ("1.0e3", F(1000.0)),
    ("1E-3", F(0.001)),
    ("-0.0", F(-0.0)),
    // The final `.*` row is an error.
    ("", E(ResolveError::JsonUnmatched)),
    ("~", E(ResolveError::JsonUnmatched)),
    ("Null", E(ResolveError::JsonUnmatched)),
    ("NULL", E(ResolveError::JsonUnmatched)),
    ("True", E(ResolveError::JsonUnmatched)),
    ("TRUE", E(ResolveError::JsonUnmatched)),
    ("+12", E(ResolveError::JsonUnmatched)),
    ("012", E(ResolveError::JsonUnmatched)),
    ("-012", E(ResolveError::JsonUnmatched)),
    ("0o17", E(ResolveError::JsonUnmatched)),
    ("0x1f", E(ResolveError::JsonUnmatched)),
    (".5", E(ResolveError::JsonUnmatched)),
    ("-.5", E(ResolveError::JsonUnmatched)),
    ("1e", E(ResolveError::JsonUnmatched)),
    ("-", E(ResolveError::JsonUnmatched)),
    (".inf", E(ResolveError::JsonUnmatched)),
    ("-.inf", E(ResolveError::JsonUnmatched)),
    (".nan", E(ResolveError::JsonUnmatched)),
    ("yes", E(ResolveError::JsonUnmatched)),
    ("hello", E(ResolveError::JsonUnmatched)),
];

#[test]
fn json_schema() {
    plain_table(&Settings::new().schema(Schema::Json), JSON);
    // 1.1 settings do not reach the JSON schema.
    plain_table(
        &Compat::yaml_1_1().schema(Schema::Json),
        &[
            ("yes", E(ResolveError::JsonUnmatched)),
            ("0777", E(ResolveError::JsonUnmatched)),
            ("1_000", E(ResolveError::JsonUnmatched)),
            ("1:30", E(ResolveError::JsonUnmatched)),
        ],
    );
}

#[test]
fn failsafe_schema() {
    let all_strings: Vec<(&str, X)> = CORE
        .iter()
        .chain(JSON)
        .chain(CORE_NO_EXT)
        .map(|(t, _)| (*t, S))
        .collect();
    plain_table(&Settings::new().schema(Schema::Failsafe), &all_strings);
    plain_table(&Compat::yaml_1_1().schema(Schema::Failsafe), &all_strings);
}

#[test]
fn only_plain_untagged_scalars_are_resolved() {
    for schema in [Schema::Core, Schema::Json, Schema::Failsafe] {
        let r = Resolver::new(&Settings::new().schema(schema));
        for text in ["12", "null", "true", ".inf", "", "hello", "~"] {
            check(
                r.resolve_scalar(TagInput::Absent, Style::NonPlain, text),
                S,
                &format!("{schema:?} quoted {text:?}"),
            );
            check(
                r.resolve_scalar(TagInput::NonSpecific, Style::Plain, text),
                S,
                &format!("{schema:?} `!` {text:?}"),
            );
        }
    }
}

fn tagged(settings: &Settings, table: &[(&str, Style, &str, X)]) {
    let r = Resolver::new(settings);
    for (tag, style, text, want) in table {
        let got = r.resolve_scalar(TagInput::Named(TagStr::from(*tag)), *style, text);
        check(
            got,
            *want,
            &format!("{:?} {tag} {style:?} {text:?}", settings.get_schema()),
        );
    }
}

#[test]
fn explicit_tags_core() {
    use Style::{NonPlain as Q, Plain as P};
    let e = ResolveError::InvalidForTag;
    tagged(
        &Settings::new(),
        &[
            (INT, Q, "12", I(12)),
            (INT, P, "0x1F", I(31)),
            (INT, Q, "0o17", I(15)),
            (INT, Q, "twelve", E(e(CoreTag::Int))),
            (INT, P, "1.5", E(e(CoreTag::Int))),
            (INT, P, "", E(e(CoreTag::Int))),
            (FLOAT, Q, "1", F(1.0)),
            (FLOAT, P, "1e3", F(1000.0)),
            (FLOAT, Q, ".nan", Nan),
            (FLOAT, Q, "-.inf", F(f64::NEG_INFINITY)),
            (FLOAT, P, "0x10", E(e(CoreTag::Float))),
            (BOOL, Q, "True", B(true)),
            (BOOL, P, "yes", E(e(CoreTag::Bool))),
            (NULL, Q, "", N),
            (NULL, P, "~", N),
            (NULL, P, "0", E(e(CoreTag::Null))),
            (STR, P, "12", S),
            (STR, P, "null", S),
            (
                SEQ,
                P,
                "x",
                E(ResolveError::CollectionTagOnScalar(CoreTag::Seq)),
            ),
            (
                MAP,
                Q,
                "x",
                E(ResolveError::CollectionTagOnScalar(CoreTag::Map)),
            ),
            ("!local", P, "12", Custom("!local")),
            (
                "tag:yaml.org,2002:binary",
                P,
                "R0lG",
                Custom("tag:yaml.org,2002:binary"),
            ),
            (
                "tag:yaml.org,2002:timestamp",
                P,
                "2001-12-14",
                Custom("tag:yaml.org,2002:timestamp"),
            ),
            (MERGE_TAG, P, "<<", Custom(MERGE_TAG)),
            ("tag:yaml.org,2002:", P, "x", Custom("tag:yaml.org,2002:")),
        ],
    );
    // 1.1 forms are valid for explicit tags too when enabled.
    tagged(
        &Compat::yaml_1_1(),
        &[
            (BOOL, P, "yes", B(true)),
            (INT, Q, "0777", I(511)),
            (INT, Q, "1:30", I(90)),
            (FLOAT, Q, "1_0.5", F(10.5)),
        ],
    );
}

#[test]
fn explicit_tags_json_and_failsafe() {
    use Style::{NonPlain as Q, Plain as P};
    let e = ResolveError::InvalidForTag;
    tagged(
        &Settings::new().schema(Schema::Json),
        &[
            (INT, Q, "12", I(12)),
            (INT, Q, "012", E(e(CoreTag::Int))),
            (FLOAT, Q, "12", F(12.0)),
            (FLOAT, Q, ".inf", E(e(CoreTag::Float))),
            (BOOL, Q, "True", E(e(CoreTag::Bool))),
            (NULL, Q, "null", N),
            (NULL, Q, "~", E(e(CoreTag::Null))),
            (STR, P, "hello", S),
            (
                SEQ,
                P,
                "x",
                E(ResolveError::CollectionTagOnScalar(CoreTag::Seq)),
            ),
            ("!x", P, "hello", Custom("!x")),
        ],
    );
    tagged(
        &Settings::new().schema(Schema::Failsafe),
        &[
            (STR, P, "12", S),
            // §10.1 knows only str, seq, map; other tags are unresolved.
            (INT, P, "12", Custom(INT)),
            (NULL, P, "", Custom(NULL)),
            (
                MAP,
                P,
                "x",
                E(ResolveError::CollectionTagOnScalar(CoreTag::Map)),
            ),
        ],
    );
}

#[test]
fn collection_tags() {
    use CollectionKind::{Mapping as M, Sequence as Q};
    let r = Resolver::new(&Settings::new());
    let ok = ResolvedCollection::Plain;
    assert_eq!(r.resolve_collection(TagInput::Absent, Q), Ok(ok));
    assert_eq!(r.resolve_collection(TagInput::NonSpecific, M), Ok(ok));
    assert_eq!(
        r.resolve_collection(TagInput::Named(TagStr::from(SEQ)), Q),
        Ok(ok)
    );
    assert_eq!(
        r.resolve_collection(TagInput::Named(TagStr::from(MAP)), M),
        Ok(ok)
    );
    for (tag, kind, ct) in [
        (MAP, Q, CoreTag::Map),
        (SEQ, M, CoreTag::Seq),
        (STR, M, CoreTag::Str),
        (INT, Q, CoreTag::Int),
    ] {
        assert_eq!(
            r.resolve_collection(TagInput::Named(TagStr::from(tag)), kind),
            Err(ResolveError::WrongTagOnCollection(ct))
        );
    }
    let set = "tag:yaml.org,2002:set";
    assert_eq!(
        r.resolve_collection(TagInput::Named(TagStr::from(set)), M),
        Ok(ResolvedCollection::Custom(TagStr::from(set)))
    );
    let fs = Resolver::new(&Settings::new().schema(Schema::Failsafe));
    assert_eq!(
        fs.resolve_collection(TagInput::Named(TagStr::from(INT)), Q),
        Ok(ResolvedCollection::Custom(TagStr::from(INT)))
    );
    assert_eq!(
        fs.resolve_collection(TagInput::Named(TagStr::from(SEQ)), M),
        Err(ResolveError::WrongTagOnCollection(CoreTag::Seq))
    );
}

#[test]
fn yaml11_booleans() {
    let s = Settings::new().yaml11_booleans(true);
    let mut table: Vec<(&str, X)> = ["y", "Y", "yes", "Yes", "YES", "on", "On", "ON"]
        .iter()
        .map(|t| (*t, B(true)))
        .chain(
            ["n", "N", "no", "No", "NO", "off", "Off", "OFF"]
                .iter()
                .map(|t| (*t, B(false))),
        )
        .collect();
    table.extend([
        ("yEs", S),
        ("oN", S),
        ("true", B(true)),
        ("FALSE", B(false)),
    ]);
    table.extend([("1_000", S), ("0777", I(777)), ("12:30:00", S)]);
    plain_table(&s, &table);
    plain_table(&s, CORE);
}

#[test]
fn legacy_octal() {
    let s = Settings::new().legacy_octal(true);
    plain_table(
        &s,
        &[
            ("0777", I(511)),
            ("-0777", I(-511)),
            ("+017", I(15)),
            ("00", I(0)),
            ("0", I(0)),
            // Not octal digits: still Core base 10 (settings are additive).
            ("08", I(8)),
            ("0o777", I(511)),
            ("0_7", S),
            ("0x1f", I(31)),
            ("yes", S),
        ],
    );
}

#[test]
fn sexagesimal() {
    let s = Settings::new().sexagesimal(true);
    plain_table(
        &s,
        &[
            // yaml.org/type/int and /float examples.
            ("190:20:30", I(685_230)),
            ("190:20:30.15", F(685_230.15)),
            ("12:30:00", I(45_000)),
            ("-1:30", I(-90)),
            ("+1:30", I(90)),
            ("1:3", I(63)),
            ("1:30.5", F(90.5)),
            ("0:30.5", F(30.5)),
            ("1:30.", F(90.0)),
            ("-1:30.5", F(-90.5)),
            ("1:60", S),
            ("1:7:", S),
            (":30", S),
            ("01:30", S),
            ("1:30:", S),
            ("1_0:30", S),
            ("1:30.5_0", S),
        ],
    );
    let wide = "9999999999999999999999999999999999999999:59";
    plain_table(
        &s,
        &[(wide, Big("9999999999999999999999999999999999999999:59"))],
    );
}

#[test]
fn digit_separators() {
    let s = Settings::new().digit_separators(true);
    plain_table(
        &s,
        &[
            ("1_000", I(1000)),
            ("1__0", I(10)),
            ("1_", I(1)),
            ("-1_000", I(-1000)),
            ("+1_000", I(1000)),
            ("_1", S),
            ("0x_ff", I(255)),
            ("0xf_f", I(255)),
            ("0x_", S),
            ("0o1_7", I(15)),
            ("0o_", S),
            ("1_000.5", F(1000.5)),
            ("1_0e1_0", S),
            ("._5", S),
            (".5_0", F(0.5)),
            ("1._5", F(1.5)),
            ("0_777", I(777)),
            ("12:30:00", S),
        ],
    );
}

#[test]
fn yaml11_combinations() {
    let all = Compat::yaml_1_1();
    plain_table(
        &all,
        &[
            ("0_777", I(511)),
            ("0_", I(0)),
            ("1_0:30", I(630)),
            ("1_0:30.5", F(630.5)),
            ("yes", B(true)),
            ("0o17", I(15)),
            // `yaml11_int_forms` (in the preset)
            // gives these Core strings an integer meaning.
            ("-0x10", I(-16)),
            ("0b101", I(5)),
            // Signed `0o` is neither 1.1 nor 1.2: it
            // stays a string even with every 1.1 behaviour on.
            ("-0o17", S),
            ("+0o17", S),
            // Signed 1.1 octal is `legacy_octal`.
            ("-017", I(-15)),
        ],
    );
    let int_forms = ["-0x10", "0b101"];
    let rest: Vec<(&str, X)> = CORE
        .iter()
        .copied()
        .filter(|(t, _)| !int_forms.contains(t))
        .collect();
    assert_eq!(rest.len() + int_forms.len(), CORE.len());
    plain_table(&all, &rest);
}

#[test]
fn yaml11_int_forms() {
    let s = Settings::new().yaml11_int_forms(true);
    plain_table(
        &s,
        &[
            ("0b101", I(5)),
            ("-0b101", I(-5)),
            ("+0b0", I(0)),
            ("-0x1F", I(-31)),
            ("+0x1f", I(31)),
            // No signed `0o` (neither 1.1 nor 1.2).
            ("-0o17", S),
            ("+0o17", S),
            // Signed legacy octal needs `legacy_octal`: decimal here.
            ("-017", I(-17)),
            ("0x1F", I(31)),
            ("0o17", I(15)),
            ("0b", S),
            ("0b102", S),
            ("0b1_0", S),
            ("-0x", S),
            ("-0o8", S),
            ("-0b", S),
            ("0777", I(777)),
            ("yes", S),
        ],
    );
    // 129 bits: kept exactly, as hexadecimal (re-reads under Core).
    let wide = format!("0b1{}", "0".repeat(128));
    plain_table(
        &s,
        &[(wide.as_str(), Big("0x100000000000000000000000000000000"))],
    );
    let wide = format!("-0b11{}", "1".repeat(127));
    plain_table(
        &s,
        &[(wide.as_str(), Big("-0x1ffffffffffffffffffffffffffffffff"))],
    );
    // With separators, `_` is allowed in the 1.1 forms too.
    let sep = s.clone().digit_separators(true);
    plain_table(&sep, &[("0b1_0", I(2)), ("-0x_1_F", I(-31))]);
    // Off (Core): all strings.
    plain_table(
        &Settings::new(),
        &[("0b101", S), ("-0x1F", S), ("+0o17", S)],
    );
}

#[test]
fn version_directive() {
    let honor = Settings::new().honor_version_directive(true);
    let r = Resolver::for_document(&honor, true);
    check(
        r.resolve_scalar(TagInput::Absent, Style::Plain, "yes"),
        B(true),
        "honored",
    );
    check(
        r.resolve_scalar(TagInput::Absent, Style::Plain, "0777"),
        I(511),
        "honored",
    );
    assert!(r.merge_keys());
    let r = Resolver::for_document(&honor, false);
    check(
        r.resolve_scalar(TagInput::Absent, Style::Plain, "yes"),
        S,
        "no directive",
    );
    assert!(!r.merge_keys());
    let r = Resolver::for_document(&Settings::new(), true);
    check(
        r.resolve_scalar(TagInput::Absent, Style::Plain, "yes"),
        S,
        "not honored",
    );
}

#[test]
fn merge_key_hook() {
    let off = Resolver::new(&Settings::new());
    assert!(!off.is_merge_key(TagInput::Absent, Style::Plain, "<<"));
    // Off: `<<` is an ordinary string.
    check(
        off.resolve_scalar(TagInput::Absent, Style::Plain, "<<"),
        S,
        "<<",
    );
    let on = Resolver::new(&Settings::new().merge_keys(true));
    assert!(on.is_merge_key(TagInput::Absent, Style::Plain, "<<"));
    assert!(on.is_merge_key(
        TagInput::Named(TagStr::from(MERGE_TAG)),
        Style::NonPlain,
        "<<"
    ));
    assert!(!on.is_merge_key(TagInput::Absent, Style::NonPlain, "<<"));
    assert!(!on.is_merge_key(TagInput::NonSpecific, Style::Plain, "<<"));
    assert!(!on.is_merge_key(TagInput::Named(TagStr::from("!x")), Style::Plain, "<<"));
    assert!(!on.is_merge_key(TagInput::Absent, Style::Plain, "<<<"));
    // Merge keys work with every schema.
    let fs = Resolver::new(&Settings::new().merge_keys(true).schema(Schema::Json));
    assert!(fs.is_merge_key(TagInput::Absent, Style::Plain, "<<"));
}

#[test]
fn settings_conflicts() {
    assert_eq!(check_settings(&Settings::new()), []);
    assert_eq!(check_settings(&Compat::yaml_1_1()), []);
    assert_eq!(check_settings(&Compat::serde_yaml()), []);
    let fs = Settings::new()
        .schema(Schema::Failsafe)
        .yaml11_booleans(true);
    let c = check_settings(&fs);
    assert_eq!(
        c,
        [Conflict::Yaml11WithoutCore {
            setting: "yaml11_booleans",
            schema: Schema::Failsafe
        }]
    );
    assert_eq!(c[0].severity(), ConflictSeverity::Warning);
    let json_all = Compat::yaml_1_1().schema(Schema::Json);
    let names: Vec<_> = check_settings(&json_all)
        .into_iter()
        .map(|c| match c {
            Conflict::Yaml11WithoutCore { setting, .. } => setting,
            other => unreachable!("{other:?}"),
        })
        .collect();
    assert_eq!(
        names,
        [
            "yaml11_booleans",
            "legacy_octal",
            "sexagesimal",
            "digit_separators",
            "yaml11_int_forms"
        ]
    );
    // merge_keys alone is meaningful with every schema.
    assert_eq!(
        check_settings(&Settings::new().merge_keys(true).schema(Schema::Failsafe)),
        []
    );
    // honor_version_directive.
    assert_eq!(
        check_settings(&Settings::new().honor_version_directive(true)),
        []
    );
    assert_eq!(
        check_settings(
            &Settings::new()
                .honor_version_directive(true)
                .schema(Schema::Json)
        ),
        [],
        "still switches merge keys on"
    );
    let c = check_settings(
        &Settings::new()
            .honor_version_directive(true)
            .merge_keys(true)
            .schema(Schema::Json),
    );
    assert_eq!(
        c,
        [Conflict::HonorDirectiveWithoutCore {
            schema: Schema::Json
        }]
    );
    let c = check_settings(&Compat::yaml_1_1().honor_version_directive(true));
    assert_eq!(c, [Conflict::HonorDirectiveRedundant]);
    assert_eq!(c[0].severity(), ConflictSeverity::Warning);
    // Incorrect output: an error.
    let c = check_settings(&Settings::new().emit(Emit::new().indent(0)));
    assert_eq!(c, [Conflict::ZeroIndent]);
    assert_eq!(c[0].severity(), ConflictSeverity::Error);
}

/// A `Schema` without a resolver (impossible today; `-core` trips a
/// test first) is never read as another schema: every node is Y9000.
#[test]
fn unsupported_schema_reports_internal() {
    let r = Resolver {
        active: Active::Unsupported,
        merge_keys: false,
        variant_content: None,
    };
    for tag in [
        TagInput::Absent,
        TagInput::NonSpecific,
        TagInput::Named(TagStr::from("tag:yaml.org,2002:int")),
    ] {
        assert_eq!(
            r.resolve_scalar(tag, Style::Plain, "1"),
            Err(ResolveError::Internal)
        );
        assert_eq!(
            r.resolve_collection(tag, CollectionKind::Mapping),
            Err(ResolveError::Internal)
        );
    }
}

/// `Number::to_core_string` re-reads under plain Core as the same integer,
/// whichever 1.1 setting produced a wide value (the emitter's contract).
#[test]
fn wide_integers_re_read_under_core() {
    let core = Resolver::new(&Settings::new());
    let read = |r: &Resolver, t: &str| match r.resolve_scalar(TagInput::Absent, Style::Plain, t) {
        Ok(Resolved::Number(n)) => n,
        other => unreachable!("{t}: {other:?}"),
    };
    let wide_hex = format!("-0x1{}", "0".repeat(40));
    let wide_oct = format!("-01{}", "7".repeat(50));
    let wide_sx = format!("-1{}:59:07", "0".repeat(40));
    let wide_bin = format!("-0b1{}", "0".repeat(200));
    let cases = [
        (Settings::new().yaml11_int_forms(true), wide_hex.as_str()),
        (Settings::new().legacy_octal(true), wide_oct.as_str()),
        (Settings::new().sexagesimal(true), wide_sx.as_str()),
        (Settings::new().yaml11_int_forms(true), wide_bin.as_str()),
        (Settings::new(), "0x1ffffffffffffffffffffffffffffffffff"),
        (
            Settings::new(),
            "-9999999999999999999999999999999999999999999",
        ),
    ];
    for (settings, text) in &cases {
        let n = read(&Resolver::new(settings), text);
        assert!(n.is_big_integer(), "{text}");
        let core_text = n.to_core_string();
        let again = read(&core, &core_text);
        assert!(again.is_integer(), "{text} -> {core_text}");
        assert_eq!(again.to_core_string(), core_text, "{text}");
    }
    // `Display` of these is not Core-safe (documented).
    let n = read(&Resolver::new(&cases[0].0), &wide_hex);
    check(
        core.resolve_scalar(TagInput::Absent, Style::Plain, &n.to_string()),
        S,
        "Display of a negative wide hex",
    );
}

/// The content of a locally tagged plain scalar, for typed enums
/// under `resolve_tagged_variant_content` only.
#[test]
fn variant_content_is_core_for_local_plain_scalars_only() {
    let on = Resolver::new(&Settings::new().resolve_tagged_variant_content(true));
    let num = |f: f64| Ok(Some(Resolved::Number(Number::from(f))));
    assert_eq!(
        on.variant_content(TagStr::from("!Square"), Style::Plain, "2.0"),
        num(2.0)
    );
    assert_eq!(
        on.variant_content(TagStr::from("!E"), Style::Plain, "true"),
        Ok(Some(Resolved::Bool(true)))
    );
    assert_eq!(
        on.variant_content(TagStr::from("!N"), Style::Plain, "~"),
        Ok(Some(Resolved::Null))
    );
    assert_eq!(
        on.variant_content(TagStr::from("!S"), Style::Plain, "x"),
        Ok(Some(Resolved::Str))
    );
    // Not plain, not local, or off: `None` (the content stays a string).
    assert_eq!(
        on.variant_content(TagStr::from("!Square"), Style::NonPlain, "2.0"),
        Ok(None)
    );
    assert_eq!(
        on.variant_content(TagStr::from("tag:example.com,2000:S"), Style::Plain, "2.0"),
        Ok(None)
    );
    assert_eq!(
        on.variant_content(TagStr::from("!"), Style::Plain, "2.0"),
        Ok(None)
    );
    let off = Resolver::new(&Settings::new());
    assert_eq!(
        off.variant_content(TagStr::from("!Square"), Style::Plain, "2.0"),
        Ok(None)
    );
    // The cheap pre-check agrees.
    assert!(on.resolves_variant_content(TagStr::from("!Square"), Style::Plain));
    assert!(!on.resolves_variant_content(TagStr::from("!Square"), Style::NonPlain));
    assert!(!on.resolves_variant_content(TagStr::from("!"), Style::Plain));
    assert!(!off.resolves_variant_content(TagStr::from("!Square"), Style::Plain));
    // Core with the document's 1.1 extensions, whatever the schema.
    let json = Resolver::new(
        &Settings::new()
            .schema(Schema::Json)
            .yaml11_int_forms(true)
            .resolve_tagged_variant_content(true),
    );
    assert_eq!(
        json.variant_content(TagStr::from("!C"), Style::Plain, "0b11"),
        Ok(Some(Resolved::Number(Number::from(3u64))))
    );
    assert_eq!(
        json.variant_content(TagStr::from("!S"), Style::Plain, "hello"),
        Ok(Some(Resolved::Str))
    );
}
