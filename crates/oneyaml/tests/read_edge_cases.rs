//! Reading and writing edge cases: floats outside the range of a double,
//! escaped surrogate pairs, and strings that look like numbers to some
//! readers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{Value, code, from_str, to_string};

#[test]
fn floats_that_round_to_infinity_or_zero_are_keys_of_their_own_value() {
    for src in [
        "1e400: a\n2e400: b\n",
        "1e400: a\n.inf: b\n",
        "-1e400: a\n-.inf: b\n",
        "1e-400: a\n2e-400: b\n",
        "1e-400: a\n0.0: b\n",
        "{1e400: a, 1e401: b, 1e402: c}\n",
    ] {
        let v: Value = from_str(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        let n = v.as_mapping().unwrap().len();
        assert!(n >= 2, "{src:?}");
    }
}

#[test]
fn the_same_out_of_range_value_is_still_a_duplicate_key() {
    for src in [
        "1e400: a\n1e400: b\n",
        "1e400: a\n10e399: b\n",
        "1e400: a\n0.1e401: b\n",
        "1e-400: a\n10e-401: b\n",
        ".inf: a\n.inf: b\n",
    ] {
        let err = from_str::<Value>(src).unwrap_err();
        assert_eq!(err.code(), code::DUPLICATE_KEY, "{src:?}");
    }
}

#[test]
fn an_out_of_range_float_still_reads_as_the_double_it_rounds_to() {
    let v: Value = from_str("1e400").unwrap();
    assert_eq!(v.as_f64(), Some(f64::INFINITY));
    let v: Value = from_str("-1e-400").unwrap();
    assert_eq!(v.as_f64().map(f64::to_bits), Some((-0.0f64).to_bits()));
    assert_eq!(from_str::<f64>("1e400").unwrap(), f64::INFINITY);
}

#[test]
fn an_out_of_range_float_is_written_so_that_it_reads_back_equal() {
    for src in ["1e400", "2.5e-400", "-7e1000", "1e99999999999999999999"] {
        let v: Value = from_str(src).unwrap();
        let text = to_string(&v).unwrap();
        assert_eq!(from_str::<Value>(&text).unwrap(), v, "{src}: {text}");
    }
}

#[test]
fn a_float_key_with_a_huge_exponent_costs_little() {
    let digits = "9".repeat(200_000);
    let src = format!("{{1e{digits}: a, 1e{digits}8: b}}\n");
    let v: Value = from_str(&src).unwrap();
    assert_eq!(v.as_mapping().unwrap().len(), 2);
}

#[test]
fn an_escaped_surrogate_pair_is_one_character() {
    let v: Value =
        from_str("a: \"\\ud83d\\ude00\"\nb: \"\\uD83D\\uDE00\"\nc: \"x\\ud83d\\ude00y\"\n")
            .unwrap();
    assert_eq!(v["a"].as_str(), Some("\u{1F600}"));
    assert_eq!(v["b"].as_str(), Some("\u{1F600}"));
    assert_eq!(v["c"].as_str(), Some("x\u{1F600}y"));
    // A lone surrogate is still not a character.
    assert!(from_str::<Value>("\"\\ud83d\"").is_err());
    assert!(from_str::<Value>("\"\\ude00\\ud83d\"").is_err());
}

#[test]
fn strings_that_some_readers_take_for_numbers_are_quoted() {
    for s in [
        "+-1", "--1", "-+1.5", "++2", "+_1", "1e3_", "0_x123", "1_0.5", "_1", "0B1", "0XFF", "0O7",
        "0X-F", "0B+1", "0b-1", "0o+7", "0b-1_0",
    ] {
        let text = to_string(&Value::from(s)).unwrap();
        assert!(
            text.starts_with(['\'', '"']),
            "{s:?} was written as {text:?}"
        );
        assert_eq!(from_str::<Value>(&text).unwrap(), Value::from(s));
    }
    // Words that only look similar stay plain.
    for s in ["+-", "+-a", "0b-", "_", "_a", "a_1"] {
        let text = to_string(&Value::from(s)).unwrap();
        assert_eq!(text, format!("{s}\n"), "{s:?}");
    }
}
