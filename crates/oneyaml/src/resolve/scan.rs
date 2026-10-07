//! Hand-written matchers for the schema regexes (YAML 1.2.2 §10.1–10.3)
//! and the YAML 1.1 type definitions (yaml.org/type), plus the value
//! builders. No regex engine; every matcher is a single linear pass
//!  over bytes, with no indexing that can panic.

use super::ResolveError;
use crate::value::{Number, Radix};

/// The YAML 1.1 behaviours that extend the Core schema. Each is
/// additive: it only gives *more* plain scalars a non-string meaning (or,
/// for `legacy_octal`, a different one for `0[0-7]+`); it never turns a
/// Core-typed scalar into a string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ext {
    pub(crate) booleans: bool,
    pub(crate) legacy_octal: bool,
    pub(crate) sexagesimal: bool,
    pub(crate) separators: bool,
    /// `yaml11_int_forms`: binary `0b`, and a sign on `0x`.
    pub(crate) int_forms: bool,
}

/// A byte cursor for the matchers.
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            b: s.as_bytes(),
            i: 0,
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn bump(&mut self) {
        self.i = self.i.saturating_add(1);
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_any(&mut self, set: &[u8]) -> bool {
        match self.peek() {
            Some(c) if set.contains(&c) => {
                self.bump();
                true
            }
            _ => false,
        }
    }
    /// Consumes `[digit]*` (or `[digit_]*` when `sep`); returns how many
    /// *digits* (not underscores) it consumed.
    fn run(&mut self, digit: fn(u8) -> bool, sep: bool) -> usize {
        let mut n = 0usize;
        while let Some(c) = self.peek() {
            if digit(c) {
                n = n.saturating_add(1);
            } else if !(sep && c == b'_') {
                break;
            }
            self.bump();
        }
        n
    }
    /// `digit (digit | _)*` with `_` only when `sep`: one leading digit,
    /// then a run. True when it matched.
    fn lead_run(&mut self, digit: fn(u8) -> bool, sep: bool) -> bool {
        match self.peek() {
            Some(c) if digit(c) => {
                self.bump();
                self.run(digit, sep);
                true
            }
            _ => false,
        }
    }
    fn done(&self) -> bool {
        self.i >= self.b.len()
    }
}

fn dec(c: u8) -> bool {
    c.is_ascii_digit()
}
fn oct(c: u8) -> bool {
    matches!(c, b'0'..=b'7')
}
fn hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}
fn bin(c: u8) -> bool {
    matches!(c, b'0' | b'1')
}

/// `[-+]?`: (negative, rest).
fn sign(s: &str) -> (bool, bool, &str) {
    if let Some(r) = s.strip_prefix('-') {
        (true, true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, true, r)
    } else {
        (false, false, s)
    }
}

/// Whole-string match of `lead_run` with nothing after it.
fn all_digits(s: &str, digit: fn(u8) -> bool, sep: bool) -> bool {
    let mut c = Cur::new(s);
    c.lead_run(digit, sep) && c.done()
}

/// Whole-string match of `(digit | _)+` with at least one digit (1.1 hex
/// and legacy octal allow a leading `_`).
fn any_digits(s: &str, digit: fn(u8) -> bool, sep: bool) -> bool {
    let mut c = Cur::new(s);
    c.run(digit, sep) > 0 && c.done()
}

// ------------------------------------------------------------------- null

/// Core §10.3.2: `null | Null | NULL | ~` and the empty scalar.
pub(crate) fn core_null(s: &str) -> bool {
    matches!(s, "" | "~" | "null" | "Null" | "NULL")
}

/// JSON §10.2.2: `null`.
pub(crate) fn json_null(s: &str) -> bool {
    s == "null"
}

// ------------------------------------------------------------------- bool

/// Core §10.3.2: `true | True | TRUE | false | False | FALSE`.
pub(crate) fn core_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// JSON §10.2.2: `true | false`.
pub(crate) fn json_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// yaml.org/type/bool (1.1): `y|Y|yes|Yes|YES|n|N|no|No|NO|true|…|on|On|ON
/// |off|Off|OFF`.
pub(crate) fn yaml11_bool(s: &str) -> Option<bool> {
    yaml11_only_bool(s).or_else(|| core_bool(s))
}

/// The YAML 1.1 bool words that are strings under Core (`yes`, `no`, `on`,
/// `off`, `y`, `n` in their 1.1 casings), with the bool they meant in 1.1.
pub(crate) fn yaml11_only_bool(s: &str) -> Option<bool> {
    match s {
        "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => Some(true),
        "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => Some(false),
        _ => None,
    }
}

/// Core bools, plus the 1.1 forms when `ext.booleans`.
pub(crate) fn ext_bool(s: &str, ext: Ext) -> Option<bool> {
    if ext.booleans {
        yaml11_bool(s)
    } else {
        core_bool(s)
    }
}

// -------------------------------------------------------------------- int

/// Builds an integer from validated `digits` (may contain `_`) in `radix`
/// (8, 10, or 16), exact when it fits `u128` magnitude, wide otherwise.
fn radix_int(neg: bool, radix: Radix, digits: &str) -> Number {
    let base: u32 = match radix {
        Radix::Oct => 8,
        Radix::Hex => 16,
        Radix::Dec | Radix::Sexagesimal => 10,
    };
    let mut mag: Option<u128> = Some(0);
    for c in digits.chars() {
        let Some(d) = c.to_digit(base) else { continue };
        mag = mag
            .and_then(|m| m.checked_mul(u128::from(base)))
            .and_then(|m| m.checked_add(u128::from(d)));
        if mag.is_none() {
            break;
        }
    }
    match mag {
        Some(m) => Number::from_parts(neg, m),
        None => {
            // Normalise: no `_`, lowercase, no leading zeros (the value is
            // non-zero because it overflowed).
            let clean: String = digits
                .chars()
                .filter(|c| *c != '_')
                .map(|c| c.to_ascii_lowercase())
                .skip_while(|c| *c == '0')
                .collect();
            Number::big(neg, radix, &clean)
        }
    }
}

/// yaml.org/type/int sexagesimal: `[1-9][0-9_]*(:[0-5]?[0-9])+` after the
/// sign (`_` only with `sep`).
fn sexagesimal_int_syntax(s: &str, sep: bool) -> bool {
    let mut c = Cur::new(s);
    if !matches!(c.peek(), Some(b'1'..=b'9')) {
        return false;
    }
    c.lead_run(dec, sep);
    let mut groups = 0usize;
    while c.eat(b':') {
        if !base60_group(&mut c) {
            return false;
        }
        groups = groups.saturating_add(1);
    }
    groups > 0 && c.done()
}

/// `[0-5]?[0-9]`.
fn base60_group(c: &mut Cur<'_>) -> bool {
    if c.eat_any(b"012345") {
        c.eat_any(b"0123456789");
        true
    } else {
        c.eat_any(b"6789")
    }
}

fn sexagesimal_int(neg: bool, s: &str) -> Number {
    let first = s.split(':').next().unwrap_or_default();
    let mut mag = radix_int(false, Radix::Dec, first).as_u128();
    for g in s.split(':').skip(1) {
        // `[0-5]?[0-9]` always fits; `None` would read as overflow (wide),
        // never as a wrong exact value.
        let d = radix_int(false, Radix::Dec, g).as_u128();
        mag = mag
            .zip(d)
            .and_then(|(m, d)| m.checked_mul(60)?.checked_add(d));
    }
    match mag {
        Some(m) => Number::from_parts(neg, m),
        None => {
            let mut text: String = first.chars().filter(|c| *c != '_').collect();
            for g in s.split(':').skip(1) {
                text.push(':');
                if g.len() < 2 {
                    text.push('0');
                }
                text.push_str(g);
            }
            Number::big(neg, Radix::Sexagesimal, &text)
        }
    }
}

/// A binary integer from validated `digits` (may contain `_`). A value too
/// wide for `u128` is kept exactly as hexadecimal digits (binary is not a
/// Core form, so the wide form must re-read without the 1.1 settings).
fn binary_int(neg: bool, digits: &str) -> Number {
    let clean: Vec<u8> = digits
        .bytes()
        .filter(|c| *c != b'_')
        .skip_while(|c| *c == b'0')
        .collect();
    let mut mag: Option<u128> = Some(0);
    for c in &clean {
        mag = mag
            .and_then(|m| m.checked_mul(2))
            .and_then(|m| m.checked_add(u128::from(*c == b'1')));
    }
    if let Some(m) = mag {
        return Number::from_parts(neg, m);
    }
    // Group bits in fours from the right.
    let pad = clean.len().wrapping_neg() % 4;
    let bits: Vec<u8> = std::iter::repeat_n(b'0', pad).chain(clean).collect();
    let hex: String = bits
        .chunks(4)
        .map(|g| {
            let v = g
                .iter()
                .fold(0u32, |a, b| a.wrapping_shl(1) | u32::from(*b == b'1'));
            char::from_digit(v, 16).unwrap_or('0')
        })
        .skip_while(|c| *c == '0')
        .collect();
    Number::big(neg, Radix::Hex, &hex)
}

/// `yaml11_int_forms` (yaml.org/type/int): `[-+]?0b[01]+` and `[-+]0x…`
/// (unsigned `0x` is Core). `_` only with separators. There is no signed
/// `0o`: `0o` is YAML 1.2 only, and 1.2 has no sign on it, so `-0o17` is
/// neither 1.1 nor 1.2 and stays a string. Signed 1.1 octal (`-017`) is
/// `legacy_octal`.
fn int_form(neg: bool, signed: bool, rest: &str, sep: bool) -> Option<Number> {
    if let Some(d) = rest.strip_prefix("0b") {
        return any_digits_core(d, bin, sep).then(|| binary_int(neg, d));
    }
    if !signed {
        return None;
    }
    if let Some(d) = rest.strip_prefix("0x") {
        return any_digits_core(d, hex, sep).then(|| radix_int(neg, Radix::Hex, d));
    }
    None
}

/// Core §10.3.2 integers, plus the enabled 1.1 forms:
/// * Core: `[-+]? [0-9]+` (base 10), `0o [0-7]+` (8), `0x [0-9a-fA-F]+` (16).
/// * `legacy_octal`: `[-+]?0[0-7]+` (1.1; `_` allowed with separators).
/// * `sexagesimal`: `[-+]?[1-9][0-9_]*(:[0-5]?[0-9])+`.
/// * `digit_separators`: `_` after the first digit of a decimal, and
///   anywhere in `0o`/`0x` digits (as 1.1 hex allows), at least one digit.
/// * `yaml11_int_forms`: `[-+]?0b[01]+`, `[-+]0x[0-9a-fA-F]+` (no signed
///   `0o`: that is neither 1.1 nor 1.2).
pub(crate) fn core_int(s: &str, ext: Ext) -> Option<Number> {
    let sep = ext.separators;
    if let Some(d) = s.strip_prefix("0o") {
        return any_digits_core(d, oct, sep).then(|| radix_int(false, Radix::Oct, d));
    }
    if let Some(d) = s.strip_prefix("0x") {
        return any_digits_core(d, hex, sep).then(|| radix_int(false, Radix::Hex, d));
    }
    let (neg, signed, rest) = sign(s);
    if ext.int_forms {
        if let Some(n) = int_form(neg, signed, rest, sep) {
            return Some(n);
        }
    }
    if ext.legacy_octal {
        if let Some(d) = rest.strip_prefix('0') {
            if any_digits(d, oct, sep) || (sep && !d.is_empty() && d.bytes().all(|c| c == b'_')) {
                return Some(radix_int(neg, Radix::Oct, d));
            }
        }
    }
    if ext.sexagesimal && sexagesimal_int_syntax(rest, sep) {
        return Some(sexagesimal_int(neg, rest));
    }
    all_digits(rest, dec, sep).then(|| radix_int(neg, Radix::Dec, rest))
}

/// `0o`/`0x` digits: Core `[digit]+`; with separators, 1.1-style
/// `[digit_]+` with at least one digit.
fn any_digits_core(d: &str, digit: fn(u8) -> bool, sep: bool) -> bool {
    if sep {
        any_digits(d, digit, true)
    } else {
        all_digits(d, digit, false)
    }
}

/// JSON §10.2.2: `-? ( 0 | [1-9] [0-9]* )`.
pub(crate) fn json_int(s: &str) -> Option<Number> {
    let neg = s.starts_with('-');
    let rest = s.strip_prefix('-').unwrap_or(s);
    let mut c = Cur::new(rest);
    json_int_part(&mut c);
    (c.i > 0 && c.done()).then(|| radix_int(neg, Radix::Dec, rest))
}

/// `0 | [1-9][0-9]*`; leaves the cursor where it stopped.
fn json_int_part(c: &mut Cur<'_>) -> bool {
    if c.eat(b'0') {
        true
    } else if matches!(c.peek(), Some(b'1'..=b'9')) {
        c.run(dec, false);
        true
    } else {
        false
    }
}

// ------------------------------------------------------------------ float

/// Parses already-validated float text (`_` removed here). A failure is an
/// invariant violation: `debug_assert!` plus `Err(Internal)`, never
/// a silent re-interpretation as a string.
fn parse_float(neg: bool, body: &str) -> Result<f64, ResolveError> {
    let clean: String = body.chars().filter(|c| *c != '_').collect();
    let v = clean.parse::<f64>().ok();
    debug_assert!(v.is_some(), "validated float text failed to parse: {clean}");
    let v = v.ok_or(ResolveError::Internal)?;
    Ok(if neg { -v } else { v })
}

/// `[eE] [-+]? [0-9]+`, optional; false if started but malformed.
fn exponent(c: &mut Cur<'_>) -> bool {
    if c.eat_any(b"eE") {
        c.eat_any(b"+-");
        c.run(dec, false) > 0
    } else {
        true
    }
}

/// Core §10.3.2 float number form (after the sign):
/// `( \. [0-9]+ | [0-9]+ ( \. [0-9]* )? ) ( [eE] [-+]? [0-9]+ )?`,
/// with `_` after the first digit of each run when `sep`.
fn core_float_syntax(s: &str, sep: bool) -> bool {
    let mut c = Cur::new(s);
    if c.eat(b'.') {
        if !c.lead_run(dec, sep) {
            return false;
        }
    } else {
        if !c.lead_run(dec, sep) {
            return false;
        }
        if c.eat(b'.') {
            c.run(dec, sep);
        }
    }
    exponent(&mut c) && c.done()
}

/// yaml.org/type/float sexagesimal: `[0-9][0-9_]*(:[0-5]?[0-9])+\.[0-9_]*`
/// after the sign (`_` only with `sep`).
fn sexagesimal_float_syntax(s: &str, sep: bool) -> bool {
    let mut c = Cur::new(s);
    if !c.lead_run(dec, sep) {
        return false;
    }
    let mut groups = 0usize;
    while c.eat(b':') {
        if !base60_group(&mut c) {
            return false;
        }
        groups = groups.saturating_add(1);
    }
    if groups == 0 || !c.eat(b'.') {
        return false;
    }
    c.run(dec, sep);
    c.done()
}

fn sexagesimal_float(neg: bool, s: &str) -> Result<f64, ResolveError> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut groups = whole.split(':');
    let mut acc = parse_float(false, groups.next().unwrap_or_default())?;
    for g in groups {
        acc = acc.mul_add(60.0, parse_float(false, g)?);
    }
    let frac: String = frac.chars().filter(|c| *c != '_').collect();
    let frac = if frac.is_empty() {
        0.0
    } else {
        parse_float(false, &format!("0.{frac}"))?
    };
    let v = acc + frac;
    Ok(if neg { -v } else { v })
}

/// Core §10.3.2 floats: the number form, `[-+]? ( \.inf | \.Inf | \.INF )`,
/// and `\.nan | \.NaN | \.NAN`; plus 1.1 sexagesimal floats when enabled.
/// `None`: not a float.
pub(crate) fn core_float(s: &str, ext: Ext) -> Option<Result<f64, ResolveError>> {
    if matches!(s, ".nan" | ".NaN" | ".NAN") {
        return Some(Ok(f64::NAN));
    }
    let (neg, _, rest) = sign(s);
    if matches!(rest, ".inf" | ".Inf" | ".INF") {
        return Some(Ok(if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }));
    }
    if core_float_syntax(rest, ext.separators) {
        return Some(parse_float(neg, rest));
    }
    if ext.sexagesimal && sexagesimal_float_syntax(rest, ext.separators) {
        return Some(sexagesimal_float(neg, rest));
    }
    None
}

/// JSON §10.2.2:
/// `-? ( 0 | [1-9] [0-9]* ) ( \. [0-9]* )? ( [eE] [-+]? [0-9]+ )?`.
pub(crate) fn json_float(s: &str) -> Option<Result<f64, ResolveError>> {
    let neg = s.starts_with('-');
    let rest = s.strip_prefix('-').unwrap_or(s);
    let mut c = Cur::new(rest);
    if !json_int_part(&mut c) {
        return None;
    }
    if c.eat(b'.') {
        c.run(dec, false);
    }
    if !(exponent(&mut c) && c.done()) {
        return None;
    }
    Some(parse_float(neg, rest))
}
