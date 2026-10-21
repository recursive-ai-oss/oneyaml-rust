//! [`Number`]: a YAML integer or float with well-defined `Eq`/`Hash`.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::OnceLock;

/// A YAML number: an integer of any width, or a float.
///
/// # Representation
///
/// * **Integers** in `-(2^128 - 1) ..= 2^128 - 1` are exact: every Rust
///   integer type (including `i128` and `u128`) round-trips.
/// * **Wider integers** are kept exactly, as normalised digits in decimal,
///   hexadecimal (octal and binary input are regrouped into hexadecimal, a
///   linear step), or sexagesimal, so a consumer can report "integer out
///   of range" precisely. [`Number::is_big_integer`] tells them apart.
///   `Display` writes them in the radix they were written in (`0o…`,
///   `0x…`, `a:b:c`, with a leading `-` when negative; binary as `0x…`).
///   That text re-reads as the same integer under Core only for decimal
///   and non-negative `0o`/`0x` values: a negative octal (YAML 1.1 `-017`)
///   has no re-readable `0o` form at all, a negative hexadecimal needs
///   `yaml11_int_forms`, and sexagesimal needs `sexagesimal`.
///   [`Number::to_core_string`] always gives a Core form every reader
///   reads as the same integer: decimal, or non-negative `0x` hex (for a
///   wide integer that must be converted to decimal and is past the
///   4,096-digit bound, the decimal text is the same integer, but it
///   re-reads as a `Number` that is **not** `==` to this one; see the
///   bound below).
///   The radix is presentation only: it never affects `==` or `Hash`.
/// * **Floats** are `f64`. A float written with a value outside the range of
///   a double (`1e400`, `1e-400`) reads as the double it rounds to (infinity,
///   zero) but keeps its exact decimal value, so two such floats are equal
///   only when they have the same value, and neither equals `.inf` or `0.0`.
///
/// # Equality and hashing
///
/// `Number` is `Eq` and `Hash` (so it can sit inside a mapping key):
///
/// * An integer never equals a float: `1` ≠ `1.0`. They are different YAML
///   values (`!!int` vs `!!float`).
/// * **All NaNs are equal to each other** (whatever their sign or payload)
///   and hash alike, so `.nan` can be a mapping key and `Eq` is reflexive.
/// * **`-0.0` and `0.0` are different values** (they emit differently and
///   `1/x` tells them apart); otherwise floats compare by value.
/// * Integers compare by **meaning only**: the
///   radix a number was written in is presentation, never part of `==` or
///   `Hash`, so `0x10` = `16` = `0o20`, and a wide `0x…` equals the same
///   integer written in decimal or base 60. A wide integer never equals an
///   exact one (its magnitude is larger than any exact integer).
///
///   **Bound:** two wide integers stored in
///   *different* radices are converted and compared by value only when
///   **both** have at most 4,096 **stored** digits. Digits are counted as
///   stored, not as written: octal and binary are stored as hexadecimal
///   (a 4,500-digit `0o…` is 3,375 stored digits, within the bound), and
///   base 60 counts its whole `a:b:c` text. Wider ones compare by
///   (stored radix, normalised digits): a 100,000-digit `0o…` does
///   **not** equal the same integer written in decimal, but does equal it
///   written in `0x` (both are stored as hexadecimal). This keeps `==` an
///   equivalence relation and every comparison linear in the digit
///   count.
///
///   Cost: hashing a wide integer is linear in its digits: a residue
///   modulo a prime drawn at random once per process (hash-flooding
///   resistance), the same for every radix. Comparing two in the
///   same radix is linear; across radices, within the bound, the residues
///   are compared first and only equal (or colliding) values are
///   converted, at most O(4,096²).
///
/// ```
/// use oneyaml::Value;
///
/// let v: Value = oneyaml::from_str(
///     "[-42, 0x1F, 1.5, .nan, -.inf, 340282366920938463463374607431768211456]",
/// )
/// .unwrap();
/// let n = |i: usize| v[i].as_number().unwrap();
/// assert!(n(0).is_integer() && n(0).is_i64() && !n(0).is_u64());
/// assert_eq!((n(0).as_i64(), n(0).as_i128(), n(0).as_f64()), (Some(-42), Some(-42), Some(-42.0)));
/// assert_eq!(n(0).to_core_string(), "-42");
/// assert_eq!(n(1).as_u128(), Some(31)); // radix is presentation only
/// assert!(n(2).is_float() && n(2).is_f64());
/// assert_eq!(n(2).as_float(), Some(1.5));
/// assert_eq!(n(1).as_float(), None); // an integer is not a float
/// assert!(n(3).is_nan() && n(4).is_infinite());
/// // 2^128: too wide for u128, kept exactly.
/// assert!(n(5).is_big_integer() && n(5).as_u128().is_none());
/// assert_eq!(n(5).to_core_string(), "340282366920938463463374607431768211456");
/// ```
#[derive(Clone)]
pub struct Number {
    n: N,
}

#[derive(Clone)]
enum N {
    /// Canonical: zero is never negative.
    Int {
        neg: bool,
        mag: u128,
    },
    Float(f64),
    /// A float outside the range of `f64`, kept with its exact value.
    OutOfRange(Box<OutOfRange>),
    Big(Box<BigInt>),
}

/// The exact value of a float that a double cannot hold: a nonzero decimal
/// value that rounds to infinity or to zero. It is `±d.ddd × 10^exp`.
#[derive(Clone, Debug)]
struct OutOfRange {
    neg: bool,
    /// The significant digits: no leading or trailing zeros, never empty.
    digits: Box<str>,
    /// The decimal exponent of the first digit, signed, no leading zeros
    /// (`0` has no sign). It may have millions of digits.
    exp: Box<str>,
    /// The double it rounds to: an infinity or a zero.
    value: f64,
}

/// `a + b` for decimal magnitudes without leading zeros.
fn add_magnitudes(a: &str, b: &str) -> String {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut out = Vec::with_capacity(a.len().max(b.len()).saturating_add(1));
    let mut carry = 0u8;
    let mut ai = a.iter().rev();
    let mut bi = b.iter().rev();
    loop {
        let (x, y) = (ai.next(), bi.next());
        if x.is_none() && y.is_none() {
            break;
        }
        let sum = x
            .map_or(0, |d| d.saturating_sub(b'0'))
            .saturating_add(y.map_or(0, |d| d.saturating_sub(b'0')))
            .saturating_add(carry);
        out.push(b'0'.saturating_add(sum % 10));
        carry = sum / 10;
    }
    if carry > 0 {
        out.push(b'0'.saturating_add(carry));
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

/// `a - b` for decimal magnitudes without leading zeros and `a >= b`.
fn sub_magnitudes(a: &str, b: &str) -> String {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0u8;
    let mut bi = b.iter().rev();
    for x in a.iter().rev() {
        let x = x.saturating_sub(b'0');
        let y = bi
            .next()
            .map_or(0, |d| d.saturating_sub(b'0'))
            .saturating_add(borrow);
        let (d, br) = if x >= y {
            (x.saturating_sub(y), 0)
        } else {
            (x.saturating_add(10).saturating_sub(y), 1)
        };
        out.push(b'0'.saturating_add(d));
        borrow = br;
    }
    while out.len() > 1 && out.last() == Some(&b'0') {
        out.pop();
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

/// Orders decimal magnitudes without leading zeros.
fn cmp_magnitudes(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// The signed decimal `(neg, mag) + k`, as a canonical signed string
/// (`0` is never negative). Linear in the digits of `mag`.
fn add_small(neg: bool, mag: &str, k: i128) -> String {
    let kneg = k < 0;
    let kmag = k.unsigned_abs().to_string();
    let (rneg, rmag) = if neg == kneg {
        (neg, add_magnitudes(mag, &kmag))
    } else {
        match cmp_magnitudes(mag, &kmag) {
            std::cmp::Ordering::Equal => (false, "0".to_owned()),
            std::cmp::Ordering::Greater => (neg, sub_magnitudes(mag, &kmag)),
            std::cmp::Ordering::Less => (kneg, sub_magnitudes(&kmag, mag)),
        }
    };
    if rneg && rmag != "0" {
        format!("-{rmag}")
    } else {
        rmag
    }
}

/// The radix a wide integer was written in, as the resolver passes it to
/// [`Number::big`] (which stores octal as hexadecimal: see [`Base`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Radix {
    Oct,
    Dec,
    Hex,
    /// Base 60 (YAML 1.1 `sexagesimal`): digits are `:`-separated groups.
    Sexagesimal,
}

/// The radix a wide integer is **stored** in: octal input is regrouped
/// into hexadecimal ([`Number::big`]), so there is no octal here and no
/// arm for it anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    Dec,
    Hex,
    /// Base 60: a decimal head, then `:`-separated groups.
    Sexagesimal,
}

/// An integer outside the exact range, kept as normalised text.
#[derive(Clone, Debug)]
struct BigInt {
    neg: bool,
    radix: Base,
    /// Written in octal: presentation only (`Display` writes `0o…`), never
    /// part of `==` or `Hash`.
    octal: bool,
    /// Lowercase, no `_`, no leading zeros (never empty, never `0`); for
    /// sexagesimal, groups separated by `:` with the later groups as written
    /// minus `_`.
    digits: Box<str>,
}

/// Canonical NaN bits used for hashing every NaN.
const NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

impl Number {
    /// An exact integer from sign and magnitude (canonicalises `-0`).
    pub(crate) const fn from_parts(neg: bool, mag: u128) -> Self {
        Self {
            n: N::Int {
                neg: neg && mag != 0,
                mag,
            },
        }
    }

    /// A wide integer; `digits` must already be normalised (see `BigInt`).
    ///
    /// Octal is regrouped into hexadecimal here (linear, as binary is by
    /// the resolver), so octal and hexadecimal always compare digit by
    /// digit, and a wide octal written as `0x` reads back as
    /// the same `Number` at any width.
    pub(crate) fn big(neg: bool, radix: Radix, digits: &str) -> Self {
        let octal = radix == Radix::Oct;
        let (radix, digits): (Base, Box<str>) = match radix {
            Radix::Oct => (Base::Hex, oct_to_hex(digits).into()),
            Radix::Hex => (Base::Hex, digits.into()),
            Radix::Dec => (Base::Dec, digits.into()),
            Radix::Sexagesimal => (Base::Sexagesimal, digits.into()),
        };
        Self {
            n: N::Big(Box::new(BigInt {
                neg,
                radix,
                octal,
                digits,
            })),
        }
    }

    /// A float whose decimal value is outside the range of a double, from
    /// the validated body `text` (digits, an optional `.` and an optional
    /// exponent, no sign, no `_`) and the double it rounds to. `None` when
    /// the value is zero, or the text is not a float body.
    pub(crate) fn out_of_range(neg: bool, text: &str, value: f64) -> Option<Self> {
        let (mantissa, exponent) = match text.find(['e', 'E']) {
            Some(i) => (text.get(..i)?, text.get(i.checked_add(1)?..)?),
            None => (text, ""),
        };
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let all: String = int.chars().chain(frac.chars()).collect();
        let trimmed = all.trim_start_matches('0');
        let digits = trimmed.trim_end_matches('0');
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let zeros = trimmed.len().saturating_sub(digits.len());
        // value = digits × 10^(exp0 - frac + zeros); the exponent of the first
        // digit adds the digits after it.
        let shift = i128::try_from(zeros)
            .ok()?
            .checked_add(i128::try_from(digits.len()).ok()?)?
            .checked_sub(1)?
            .checked_sub(i128::try_from(frac.len()).ok()?)?;
        let (eneg, emag) = match exponent.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, exponent.strip_prefix('+').unwrap_or(exponent)),
        };
        let emag = emag.trim_start_matches('0');
        let emag = if emag.is_empty() { "0" } else { emag };
        if !emag.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            n: N::OutOfRange(Box::new(OutOfRange {
                neg,
                digits: digits.into(),
                exp: add_small(eneg, emag, shift).into(),
                value,
            })),
        })
    }

    /// True for a float that a double cannot hold (see the type docs).
    pub(crate) fn is_out_of_range(&self) -> bool {
        matches!(self.n, N::OutOfRange(_))
    }

    /// True for an integer (exact or wide).
    #[must_use]
    pub fn is_integer(&self) -> bool {
        !matches!(self.n, N::Float(_) | N::OutOfRange(_))
    }

    /// True for an integer too wide for `i128`/`u128`.
    #[must_use]
    pub fn is_big_integer(&self) -> bool {
        matches!(self.n, N::Big(_))
    }

    /// True for a float (including infinities and NaN).
    #[must_use]
    pub fn is_float(&self) -> bool {
        matches!(self.n, N::Float(_) | N::OutOfRange(_))
    }

    /// True for an integer that fits `i64`.
    #[must_use]
    pub fn is_i64(&self) -> bool {
        self.as_i64().is_some()
    }

    /// True for an integer that fits `u64`.
    #[must_use]
    pub fn is_u64(&self) -> bool {
        self.as_u64().is_some()
    }

    /// True for a float (same as [`Number::is_float`], `serde_yaml` name).
    #[must_use]
    pub fn is_f64(&self) -> bool {
        self.is_float()
    }

    /// True for a NaN float.
    #[must_use]
    pub fn is_nan(&self) -> bool {
        matches!(self.n, N::Float(f) if f.is_nan())
    }

    /// True for a positive or negative infinite float.
    #[must_use]
    pub fn is_infinite(&self) -> bool {
        match &self.n {
            N::Float(f) => f.is_infinite(),
            N::OutOfRange(w) => w.value.is_infinite(),
            _ => false,
        }
    }

    /// The integer as `i128`, if it is one and fits.
    #[must_use]
    pub fn as_i128(&self) -> Option<i128> {
        match self.n {
            N::Int { neg: false, mag } => i128::try_from(mag).ok(),
            N::Int { neg: true, mag } => 0i128.checked_sub_unsigned(mag),
            _ => None,
        }
    }

    /// The integer as `u128`, if it is one and fits.
    #[must_use]
    pub fn as_u128(&self) -> Option<u128> {
        match self.n {
            N::Int { neg: false, mag } => Some(mag),
            _ => None,
        }
    }

    /// The integer as `i64`, if it is one and fits.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.as_i128().and_then(|v| i64::try_from(v).ok())
    }

    /// The integer as `u64`, if it is one and fits.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.as_u128().and_then(|v| u64::try_from(v).ok())
    }

    /// The number as `f64`: floats as they are, exact integers rounded to
    /// the nearest `f64`; `None` for wide integers.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match &self.n {
            N::Float(f) => Some(*f),
            N::OutOfRange(w) => Some(w.value),
            N::Int { neg, mag } => {
                let f = *mag as f64;
                Some(if *neg { -f } else { f })
            }
            N::Big(_) => None,
        }
    }

    /// The float, only if this number is a float.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match &self.n {
            N::Float(f) => Some(*f),
            N::OutOfRange(w) => Some(w.value),
            _ => None,
        }
    }
}

impl Number {
    /// The number as text that the YAML 1.2 **Core** schema reads back as
    /// this same number, whatever settings it was read with (for the
    /// emitter). Integers are written in decimal or as non-negative
    /// `0x` hex only, never `0o` (YAML 1.1 readers read `0o…`
    /// as a string).
    ///
    /// Same as `Display` for exact integers, floats, decimal wide integers,
    /// and non-negative `0x` wide integers. Otherwise:
    ///
    /// * a non-negative wide octal (stored as hexadecimal) is written as
    ///   `0x` hex, like any non-negative wide hexadecimal: linear;
    /// * a negative wide octal or hexadecimal (YAML 1.1 `-017`, `-0x1F`)
    ///   and a sexagesimal one **of either sign** have no linear Core form
    ///   (`-0x…` is not Core, and base 60 is not Core at all), so they are
    ///   converted to decimal, which costs O(d²) time in their digit count
    ///   `d` (schoolbook base conversion; `d` is bounded by
    ///   `Limits::max_scalar_len`).
    ///
    /// This method always converts, at any width: the text is the same
    /// integer, but past the 4,096-digit bound (stored digits, or digits
    /// of the decimal result; see the type docs) it re-reads as a decimal
    /// `Number` that is **not** `==` to this one, since cross-radix
    /// equality stops at that bound. The emitter converts only within the
    /// bound and refuses the rest (negative non-decimal, and sexagesimal of
    /// any sign) with `Y3015` (emitting stays linear),
    /// so what it writes always reads back as an equal `Number`.
    #[must_use]
    pub fn to_core_string(&self) -> String {
        let mut out = String::new();
        // No digit count exceeds `usize::MAX`: never `Err`.
        let _ = self.write_core(&mut out, usize::MAX);
        out
    }

    /// Appends [`Number::to_core_string`] to `out` (no per-number
    /// buffer for exact numbers). A wide integer that needs a base
    /// conversion (see there) is converted only when it and its decimal
    /// form have at most `max_converted` digits; otherwise `Err(())` and
    /// nothing is written.
    pub(crate) fn write_core(&self, out: &mut String, max_converted: usize) -> Result<(), ()> {
        use std::fmt::Write as _;
        match &self.n {
            N::Big(b) if !b.neg && b.radix == Base::Hex => {
                // Non-negative non-decimal wide integers
                // (octal and binary are stored as hexadecimal) as `0x`,
                // linear in the digits.
                out.push_str("0x");
                out.push_str(&b.digits);
            }
            N::Big(b) if b.radix != Base::Dec => {
                // Both the source and the decimal result within the bound:
                // the output then reads back as a decimal `Number` equal
                // to this one (cross-radix equality has the same bound).
                if b.digits.len() > max_converted {
                    return Err(());
                }
                let decimal = b.decimal();
                if decimal.len() > max_converted {
                    return Err(());
                }
                if b.neg {
                    out.push('-');
                }
                out.push_str(&decimal);
            }
            _ => {
                let _ = write!(out, "{self}");
            }
        }
        Ok(())
    }
}

/// Wide integers in different radices are converted
/// (to compare, or to write in decimal) only up to this many digits.
pub(crate) const MAX_CONVERTED_DIGITS: usize = 4096;

/// The per-process random prime of [`BigInt::residue`], in `[2^60, 2^61)`
/// (hash-flooding resistance; chosen with `std`'s `RandomState`, no
/// extra dependency). Crafted keys cannot aim at one residue class
/// without knowing it.
fn residue_modulus() -> u64 {
    static PRIME: OnceLock<u64> = OnceLock::new();
    *PRIME.get_or_init(|| {
        let seed = RandomState::new().hash_one(0x6f6e_6579_616d_6c00_u64);
        let low = 1u64 << 60;
        let mut c = (seed & (low - 1)) | low | 1;
        // Primes are ~1 in 42 around 2^60: a few dozen odd candidates.
        while !is_prime(c) {
            c = c.wrapping_add(2);
            if c >= low << 1 {
                c = low | 1;
            }
        }
        c
    })
}

fn mul_mod(a: u64, b: u64, m: u64) -> u64 {
    let p = u128::from(a).wrapping_mul(u128::from(b));
    // `m > 0` (an odd candidate); the remainder is below `m`.
    u64::try_from(p.checked_rem(u128::from(m)).unwrap_or(0)).unwrap_or(0)
}

fn pow_mod(mut base: u64, mut exp: u64, m: u64) -> u64 {
    let mut acc = 1u64;
    while exp > 0 {
        if exp & 1 == 1 {
            acc = mul_mod(acc, base, m);
        }
        base = mul_mod(base, base, m);
        exp >>= 1;
    }
    acc
}

/// Deterministic Miller–Rabin for odd `n > 37` (these bases are exact for
/// every `u64`).
fn is_prime(n: u64) -> bool {
    const BASES: [u64; 12] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];
    if n < 41 || n & 1 == 0 {
        return BASES.contains(&n);
    }
    let d0 = n.wrapping_sub(1);
    let s = d0.trailing_zeros();
    let d = d0 >> s;
    'bases: for a in BASES {
        let mut x = pow_mod(a, d, n);
        if x == 1 || x == d0 {
            continue;
        }
        for _ in 1..s {
            x = mul_mod(x, x, n);
            if x == d0 {
                continue 'bases;
            }
        }
        return false;
    }
    true
}

impl BigInt {
    /// The magnitude's digits in decimal: O(d²) for other radices.
    fn decimal(&self) -> std::borrow::Cow<'_, str> {
        match self.radix {
            Base::Dec => std::borrow::Cow::Borrowed(&*self.digits),
            Base::Hex => to_decimal(&self.digits, 16).into(),
            Base::Sexagesimal => sexagesimal_to_decimal(&self.digits).into(),
        }
    }

    /// The magnitude modulo the per-process prime ([`residue_modulus`]),
    /// radix-independent, in one pass (linear).
    fn residue(&self) -> u64 {
        self.residue_mod(residue_modulus())
    }

    fn residue_mod(&self, m: u64) -> u64 {
        let m = u128::from(m);
        // `acc < m < 2^61` and `radix ≤ 60`: no overflow in `u128`.
        let fold = |acc: u128, radix: u128, d: u32| {
            acc.wrapping_mul(radix)
                .wrapping_add(u128::from(d))
                .checked_rem(m)
                .unwrap_or(0)
        };
        let acc = match self.radix {
            Base::Dec | Base::Hex => {
                let radix: u32 = if self.radix == Base::Hex { 16 } else { 10 };
                self.digits
                    .chars()
                    .filter_map(|c| c.to_digit(radix))
                    .fold(0, |acc, d| fold(acc, u128::from(radix), d))
            }
            Base::Sexagesimal => {
                let mut groups = self.digits.split(':');
                let head = groups.next().unwrap_or_default();
                let acc = head
                    .chars()
                    .filter_map(|c| c.to_digit(10))
                    .fold(0, |acc, d| fold(acc, 10, d));
                groups.fold(acc, |acc, g| {
                    let v = g
                        .chars()
                        .filter_map(|c| c.to_digit(10))
                        .fold(0u32, |a, d| a.wrapping_mul(10).wrapping_add(d))
                        .min(59);
                    fold(acc, 60, v)
                })
            }
        };
        // `acc < m < 2^61`: the conversion never truncates.
        u64::try_from(acc).unwrap_or(u64::MAX)
    }

    /// Same integer, whatever the radices (meaning only),
    /// within the conversion bound: across stored radices, only when both
    /// have at most [`MAX_CONVERTED_DIGITS`] stored digits; wider ones
    /// compare by (stored radix, normalised digits). Linear in the digits
    /// beyond the bound.
    fn same_value(&self, other: &Self) -> bool {
        if self.neg != other.neg {
            return false;
        }
        if self.radix == other.radix {
            // Normalised digits: one spelling per value and radix.
            return self.digits == other.digits;
        }
        if self.digits.len() > MAX_CONVERTED_DIGITS || other.digits.len() > MAX_CONVERTED_DIGITS {
            return false;
        }
        if self.residue() != other.residue() {
            return false;
        }
        // Different stored radices: at most one is hexadecimal (octal is
        // stored as hexadecimal), so decimal is the common form.
        self.decimal() == other.decimal()
    }
}

/// Octal digits as normalised hexadecimal digits: a linear regrouping of
/// the bits through a small accumulator, one output byte per digit
/// and no intermediate buffer.
fn oct_to_hex(digits: &str) -> String {
    regroup(digits, 8, 16)
}

/// Normalised hexadecimal digits as octal digits (for `Display` of a
/// number written in octal): linear, like [`oct_to_hex`].
fn hex_to_oct(digits: &str) -> String {
    regroup(digits, 16, 8)
}

/// `digits` in radix `from` (8 or 16) as normalised digits in radix `to`
/// (16 or 8): the bit string regrouped from `from`'s digit width into
/// `to`'s, left to right. Leading zero bits pad the front to whole output
/// digits; leading zero output digits are dropped (so zero is empty, as
/// before). Characters that are not digits of `from` are skipped.
fn regroup(digits: &str, from: u32, to: u32) -> String {
    // Bits per digit: 3 (octal) or 4 (hexadecimal).
    let (in_bits, out_bits) = (from.trailing_zeros(), to.trailing_zeros());
    let count = digits.chars().filter(|c| c.is_digit(from)).count();
    // Total bits `count × in_bits`, padded to a multiple of `out_bits`;
    // computed modulo `out_bits` so the product never overflows.
    let rem = (count % 12)
        .wrapping_mul(in_bits as usize)
        .checked_rem(out_bits as usize)
        .unwrap_or(0);
    let pad = (out_bits as usize)
        .wrapping_sub(rem)
        .checked_rem(out_bits as usize)
        .unwrap_or(0);
    let mut out = String::with_capacity(
        count
            .saturating_mul(in_bits as usize)
            .checked_div(out_bits as usize)
            .unwrap_or(0)
            .saturating_add(1),
    );
    // `acc` holds `held < out_bits + in_bits ≤ 7` pending bits.
    let (mut acc, mut held) = (0u32, u32::try_from(pad).unwrap_or(0));
    for d in digits.chars().filter_map(|c| c.to_digit(from)) {
        acc = acc.wrapping_shl(in_bits) | d;
        held = held.wrapping_add(in_bits);
        while held >= out_bits {
            held = held.wrapping_sub(out_bits);
            let v = acc.wrapping_shr(held);
            acc &= 1u32.wrapping_shl(held).wrapping_sub(1);
            if v != 0 || !out.is_empty() {
                out.push(char::from_digit(v, to).unwrap_or('0'));
            }
        }
    }
    // No invariant check is needed here, because `held == 0` holds
    // for every input. `count` and the loop accept exactly the same
    // characters (`is_digit(from)` ⇔ `to_digit(from).is_some()`), so the
    // loop adds `count × in_bits` bits to the initial `pad`, and `pad` was
    // chosen so that `pad + count × in_bits ≡ 0 (mod out_bits)` (the
    // `% 12` keeps the residue: 12 is a multiple of both 3 and 4). The
    // inner `while` drains whole output digits, so `held` ends at
    // `(pad + count × in_bits) mod out_bits = 0`: no bit is left over.
    // `regrouping_matches_u128_formatting` checks the result.
    out
}

/// Little-endian base-10⁹ limbs.
const LIMB: u64 = 1_000_000_000;

/// `limbs = limbs × mul + add`, with `mul ≤ 2³²` and `add < mul`, so
/// every intermediate stays below `10⁹ × 2³² + 2³² < 2⁶⁴`.
fn mul_add(limbs: &mut Vec<u64>, mul: u64, add: u64) {
    let mut carry = add;
    for limb in limbs.iter_mut() {
        let t = limb.wrapping_mul(mul).wrapping_add(carry);
        *limb = t % LIMB;
        carry = t / LIMB;
    }
    while carry > 0 {
        limbs.push(carry % LIMB);
        carry /= LIMB;
    }
}

/// Folds `digit`s (values below `radix ≤ 60`) into `limbs`, several per
/// multiplication.
fn fold_digits(limbs: &mut Vec<u64>, radix: u64, digits: impl Iterator<Item = u64>) {
    // Largest power of `radix` that keeps `mul ≤ 2³²`.
    let (mut chunk, mut mul) = (0u64, 1u64);
    for d in digits {
        if mul.saturating_mul(radix) > 1 << 32 {
            mul_add(limbs, mul, chunk);
            (chunk, mul) = (0, 1);
        }
        chunk = chunk.wrapping_mul(radix).wrapping_add(d);
        mul = mul.wrapping_mul(radix);
    }
    if mul > 1 {
        mul_add(limbs, mul, chunk);
    }
}

fn limbs_to_string(limbs: &[u64]) -> String {
    let mut out = String::new();
    let mut rest = limbs.iter().rev().skip_while(|l| **l == 0);
    match rest.next() {
        None => return "0".to_owned(),
        Some(top) => out.push_str(&top.to_string()),
    }
    for limb in rest {
        out.push_str(&format!("{limb:09}"));
    }
    out
}

/// Normalised `digits` in `radix` (8 or 16) as decimal digits.
fn to_decimal(digits: &str, radix: u32) -> String {
    let mut limbs = Vec::new();
    let values = digits
        .chars()
        .filter_map(|c| c.to_digit(radix))
        .map(u64::from);
    fold_digits(&mut limbs, u64::from(radix), values);
    limbs_to_string(&limbs)
}

/// Normalised sexagesimal `a:b:c` (a decimal head, then base-60 groups) as
/// decimal digits.
fn sexagesimal_to_decimal(digits: &str) -> String {
    let mut limbs = Vec::new();
    let mut groups = digits.split(':');
    let head = groups.next().unwrap_or_default();
    fold_digits(
        &mut limbs,
        10,
        head.chars().filter_map(|c| c.to_digit(10)).map(u64::from),
    );
    let base60 = groups.map(|g| {
        g.chars()
            .filter_map(|c| c.to_digit(10))
            .fold(0u64, |a, d| a.wrapping_mul(10).wrapping_add(u64::from(d)))
            .min(59)
    });
    fold_digits(&mut limbs, 60, base60);
    limbs_to_string(&limbs)
}

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        match (&self.n, &other.n) {
            (N::Int { neg: a, mag: x }, N::Int { neg: b, mag: y }) => a == b && x == y,
            // NaN == NaN; -0.0 != 0.0; otherwise by value (bits are equal
            // exactly when non-NaN values are equal and signs of zero match).
            (N::Float(a), N::Float(b)) => (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits(),
            (N::Big(a), N::Big(b)) => a.same_value(b),
            // By exact value: `1e400` and `2e400` are different numbers
            // although both round to infinity, and neither is `.inf`.
            (N::OutOfRange(a), N::OutOfRange(b)) => {
                a.neg == b.neg && a.digits == b.digits && a.exp == b.exp
            }
            _ => false,
        }
    }
}

impl Eq for Number {}

impl Hash for Number {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.n {
            N::Int { neg, mag } => {
                state.write_u8(0);
                neg.hash(state);
                mag.hash(state);
            }
            N::Float(f) => {
                state.write_u8(1);
                let bits = if f.is_nan() { NAN_BITS } else { f.to_bits() };
                bits.hash(state);
            }
            // Meaning only: sign and a radix-independent residue modulo a
            // per-process random prime (crafted keys cannot collide
            // on purpose), so equal values hash alike in every radix.
            N::Big(b) => {
                state.write_u8(2);
                b.neg.hash(state);
                state.write_u64(b.residue());
            }
            N::OutOfRange(w) => {
                state.write_u8(3);
                w.neg.hash(state);
                w.digits.hash(state);
                w.exp.hash(state);
            }
        }
    }
}

impl fmt::Display for Number {
    /// Integers in decimal (wide ones in their source radix, as `0o…`,
    /// `0x…`, or `a:b:c`, which may not re-read under Core; see the type
    /// docs and [`Number::to_core_string`]); floats as `.inf`, `-.inf`,
    /// `.nan`, or the shortest round-trip form, which always has a `.` or
    /// an exponent.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.n {
            N::Int { neg, mag } => {
                if *neg {
                    f.write_str("-")?;
                }
                write!(f, "{mag}")
            }
            N::Float(x) if x.is_nan() => f.write_str(".nan"),
            N::Float(x) if x.is_infinite() => f.write_str(if x.is_sign_negative() {
                "-.inf"
            } else {
                ".inf"
            }),
            // `{:?}` is the shortest round-trip form and always keeps a `.`
            // or an exponent (`1.0`, `1e300`).
            N::Float(x) => write!(f, "{x:?}"),
            // The exact value as `d.ddde+X`: a `.` and a signed exponent,
            // which every reader reads as a float.
            N::OutOfRange(w) => {
                let cut = w.digits.len().min(1);
                let first = w.digits.get(..cut).unwrap_or("");
                let rest = w.digits.get(cut..).unwrap_or("");
                let rest = if rest.is_empty() { "0" } else { rest };
                let sign = if w.neg { "-" } else { "" };
                let exp = if w.exp.starts_with('-') {
                    w.exp.to_string()
                } else {
                    format!("+{}", w.exp)
                };
                write!(f, "{sign}{first}.{rest}e{exp}")
            }
            N::Big(b) => {
                if b.neg {
                    f.write_str("-")?;
                }
                if b.octal {
                    return write!(f, "0o{}", hex_to_oct(&b.digits));
                }
                let prefix = match b.radix {
                    Base::Hex => "0x",
                    Base::Dec | Base::Sexagesimal => "",
                };
                write!(f, "{prefix}{}", b.digits)
            }
        }
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Number({self})")
    }
}

macro_rules! from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Number {
            fn from(v: $t) -> Self {
                Self::from_parts(v < 0, u128::from(v.unsigned_abs()))
            }
        }
    )*};
}
macro_rules! from_uint {
    ($($t:ty),*) => {$(
        impl From<$t> for Number {
            fn from(v: $t) -> Self {
                Self::from_parts(false, u128::from(v))
            }
        }
    )*};
}
from_int!(i8, i16, i32, i64, i128);
from_uint!(u8, u16, u32, u64, u128);

impl From<isize> for Number {
    fn from(v: isize) -> Self {
        // usize → u128 is lossless on every supported target.
        Self::from_parts(v < 0, v.unsigned_abs() as u128)
    }
}

impl From<usize> for Number {
    fn from(v: usize) -> Self {
        Self::from_parts(false, v as u128)
    }
}

impl From<f64> for Number {
    fn from(v: f64) -> Self {
        Self { n: N::Float(v) }
    }
}

impl From<f32> for Number {
    fn from(v: f32) -> Self {
        Self::from(f64::from(v))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;

    fn h(n: &Number) -> u64 {
        // A fixed RandomState so hashes are comparable within the test.
        thread_local!(static S: RandomState = RandomState::new());
        S.with(|s| s.hash_one(n))
    }

    #[test]
    fn integer_ranges() {
        assert_eq!(Number::from(i128::MIN).as_i128(), Some(i128::MIN));
        assert_eq!(Number::from(u128::MAX).as_u128(), Some(u128::MAX));
        assert_eq!(Number::from(u128::MAX).as_i128(), None);
        assert_eq!(Number::from(-1i8).as_u64(), None);
        assert_eq!(Number::from(-1i8).as_i64(), Some(-1));
        assert_eq!(Number::from(u64::MAX).as_i64(), None);
        assert!(Number::from(5u8).is_u64() && Number::from(5u8).is_i64());
        assert_eq!(Number::from_parts(true, 0), Number::from(0));
        assert_eq!(Number::from(-3).as_f64(), Some(-3.0));
        assert_eq!(Number::from(3).as_float(), None);
    }

    #[test]
    fn nan_and_signed_zero() {
        let a = Number::from(f64::NAN);
        let b = Number::from(-f64::NAN);
        assert_eq!(a, b);
        assert_eq!(h(&a), h(&b));
        assert!(a.is_nan() && a.is_float() && !a.is_integer());
        assert_ne!(Number::from(0.0), Number::from(-0.0));
        assert_eq!(Number::from(1.5), Number::from(1.5));
        assert_ne!(Number::from(1), Number::from(1.0));
        assert_ne!(h(&Number::from(1)), h(&Number::from(1.0)));
    }

    /// `==`/`Hash` are meaning-only; the radix is presentation.
    #[test]
    fn wide_integers_compare_by_value_across_radices() {
        // 2^128 in every radix.
        let p128 = "340282366920938463463374607431768211456";
        let forms = [
            Number::big(false, Radix::Dec, p128),
            Number::big(false, Radix::Hex, "100000000000000000000000000000000"),
            Number::big(
                false,
                Radix::Oct,
                "4000000000000000000000000000000000000000000",
            ),
            Number::big(
                false,
                Radix::Sexagesimal,
                "94522879700260684295381835397713392:04:16",
            ),
        ];
        for a in &forms {
            for b in &forms {
                assert_eq!(a, b, "{a:?} vs {b:?}");
                assert_eq!(h(a), h(b), "{a:?} vs {b:?}");
            }
        }
        // 2^128 + 1, and the negative, differ from every form.
        let other = [
            Number::big(false, Radix::Dec, "340282366920938463463374607431768211457"),
            Number::big(false, Radix::Hex, "100000000000000000000000000000001"),
            Number::big(
                false,
                Radix::Oct,
                "4000000000000000000000000000000000000000001",
            ),
            Number::big(true, Radix::Dec, p128),
            Number::big(true, Radix::Hex, "100000000000000000000000000000000"),
        ];
        for a in &forms {
            for b in &other {
                assert_ne!(a, b, "{a:?} vs {b:?}");
            }
        }
        // Values congruent modulo the process's prime get past the residue
        // check and are still told apart exactly.
        let m = u128::from(residue_modulus());
        let x = Number::big(false, Radix::Hex, "100000000000000000000000000000000");
        // 2^128 + m in decimal (no carry out of the 30-digit tail).
        let y_dec = {
            let (head, tail) = p128.split_at(p128.len() - 30);
            let t: u128 = tail.parse().unwrap();
            format!("{head}{:030}", t + m)
        };
        let y = Number::big(false, Radix::Dec, &y_dec);
        assert_eq!(x.as_big().unwrap().residue(), y.as_big().unwrap().residue());
        assert_ne!(x, y);
        // A wide integer never equals an exact one.
        assert_ne!(forms[0], Number::from(u128::MAX));
    }

    impl Number {
        fn as_big(&self) -> Option<&BigInt> {
            match &self.n {
                N::Big(b) => Some(b),
                _ => None,
            }
        }
    }

    #[test]
    fn display() {
        assert_eq!(Number::from(-12).to_string(), "-12");
        assert_eq!(Number::from(1.0).to_string(), "1.0");
        assert_eq!(Number::from(1e300).to_string(), "1e300");
        assert_eq!(Number::from(f64::NEG_INFINITY).to_string(), "-.inf");
        assert_eq!(Number::from(f64::INFINITY).to_string(), ".inf");
        assert_eq!(Number::from(f64::NAN).to_string(), ".nan");
        let big = Number::big(true, Radix::Hex, "1ffffffffffffffffffffffffffffffff");
        assert_eq!(big.to_string(), "-0x1ffffffffffffffffffffffffffffffff");
        assert!(big.is_big_integer() && big.is_integer());
        assert_eq!(big.as_i128(), None);
        assert_eq!(big.as_f64(), None);
        assert_eq!(format!("{:?}", Number::from(2)), "Number(2)");
    }

    #[test]
    fn core_string_of_wide_integers() {
        // 2^128 = 340282366920938463463374607431768211456.
        let p128 = "340282366920938463463374607431768211456";
        let hex = Number::big(true, Radix::Hex, "100000000000000000000000000000000");
        assert_eq!(hex.to_core_string(), format!("-{p128}"));
        let oct = Number::big(
            true,
            Radix::Oct,
            "4000000000000000000000000000000000000000000",
        );
        assert_eq!(oct.to_core_string(), format!("-{p128}"));
        // Non-negative 0x and decimal are Core already: unchanged.
        let pos = Number::big(false, Radix::Hex, "100000000000000000000000000000000");
        assert_eq!(pos.to_core_string(), pos.to_string());
        // Never `0o` (1.1 readers read it as a string).
        let pos_oct = Number::big(
            false,
            Radix::Oct,
            "4000000000000000000000000000000000000000000",
        );
        // `Display` keeps the octal presentation.
        assert_eq!(
            pos_oct.to_string(),
            "0o4000000000000000000000000000000000000000000"
        );
        assert_eq!(hex_to_oct("1ff"), "777");
        assert_eq!(hex_to_oct("8"), "10");
        // Non-negative wide octal as `0x` (linear).
        assert_eq!(
            pos_oct.to_core_string(),
            "0x100000000000000000000000000000000"
        );
        let dec = Number::big(true, Radix::Dec, p128);
        assert_eq!(dec.to_core_string(), format!("-{p128}"));
        // Sexagesimal: 2^128 = h:mm:ss with h = 2^128 div 3600 etc.
        // (340282366920938463463374607431768211456 = 94522879700260684295381835397713392 * 3600 + 256)
        let sx = Number::big(
            false,
            Radix::Sexagesimal,
            "94522879700260684295381835397713392:04:16",
        );
        assert_eq!(sx.to_core_string(), p128);
        assert_eq!(
            Number::big(
                true,
                Radix::Sexagesimal,
                "94522879700260684295381835397713392:04:16"
            )
            .to_core_string(),
            format!("-{p128}")
        );
        // Exact values and floats are as `Display`.
        assert_eq!(Number::from(-5).to_core_string(), "-5");
        assert_eq!(Number::from(1.0).to_core_string(), "1.0");
        assert_eq!(Number::from(f64::NAN).to_core_string(), ".nan");
        // Limb boundaries.
        assert_eq!(to_decimal("3b9aca00", 16), "1000000000");
        assert_eq!(to_decimal("3b9ac9ff", 16), "999999999");
        assert_eq!(to_decimal("", 16), "0");
        assert_eq!(
            to_decimal(&"f".repeat(64), 16),
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        );
    }

    /// The hash residue is taken modulo a
    /// per-process random prime, so keys crafted to collide modulo a fixed
    /// modulus (here 2^61 - 1, the old one) hash apart.
    #[test]
    fn residue_modulus_is_a_random_prime() {
        let p = residue_modulus();
        assert!((1u64 << 60..1u64 << 61).contains(&p));
        assert!(is_prime(p));
        assert_eq!(p, residue_modulus());
        for (n, prime) in [
            (2, true),
            (37, true),
            (41, true),
            (561, false),
            ((1u64 << 61) - 1, true),
            ((1u64 << 61) + 1, false),
            (4_294_967_297, false),
            (18_446_744_073_709_551_557, true),
        ] {
            assert_eq!(is_prime(n), prime, "{n}");
        }
        // 10^45 + k(2^61 - 1): one residue class of the old modulus.
        let old = (1u128 << 61) - 1;
        let colliding: Vec<Number> = (0..2000u128)
            .map(|k| {
                let d = format!("1{:045}", k * old);
                Number::big(false, Radix::Dec, &d[d.len() - 46..])
            })
            .collect();
        let old_residues: std::collections::HashSet<u64> = colliding
            .iter()
            .map(|n| n.as_big().unwrap().residue_mod((1 << 61) - 1))
            .collect();
        assert_eq!(old_residues.len(), 1, "the keys do collide mod 2^61 - 1");
        let hashes: std::collections::HashSet<u64> = colliding.iter().map(h).collect();
        assert!(hashes.len() > 1990, "{} distinct hashes", hashes.len());
    }

    /// Across radices, values are
    /// converted only when **both** sides have at most 4,096 digits.
    #[test]
    fn cross_radix_equality_is_bounded() {
        // 16^(n-1) as `0x1` + zeros, and in decimal.
        let pair = |hex_digits: usize| {
            let hex = format!("1{}", "0".repeat(hex_digits - 1));
            let dec = to_decimal(&hex, 16);
            (
                Number::big(false, Radix::Hex, &hex),
                Number::big(false, Radix::Dec, &dec),
                dec.len(),
            )
        };
        // 3,000 hex digits = 3,612 decimal digits: both within the bound.
        let (x, d, len) = pair(3000);
        assert!(len <= MAX_CONVERTED_DIGITS);
        assert_eq!(x, d);
        assert_eq!(h(&x), h(&d));
        // 3,500 hex digits (within) = 4,214 decimal digits (beyond): the
        // decimal side is too wide, so they compare by (radix, digits).
        let (x, d, len) = pair(3500);
        assert!(len > MAX_CONVERTED_DIGITS);
        assert_ne!(x, d, "one side beyond the bound: not converted");
        assert_eq!(x, x.clone());
        // Hash stays consistent with `==` (equal values hash alike anyway).
        assert_eq!(h(&x), h(&d));
        // Octal is stored as hexadecimal: octal and hexadecimal compare
        // digit by digit at any width.
        let oct = Number::big(false, Radix::Oct, &format!("1{}", "0".repeat(9999)));
        let hex = Number::big(false, Radix::Hex, &format!("2{}", "0".repeat(7499)));
        assert_eq!(oct, hex);
    }

    /// A 100,000-digit cross-radix pair compares in
    /// linear time. The pair is the **same** integer (8^99999 in octal and
    /// in decimal), so the residues agree: before the bound, every
    /// comparison converted the octal side to decimal, O(d²), over a
    /// second each here.
    #[test]
    fn wide_cross_radix_pair_compares_in_linear_time() {
        let digits = 100_000;
        let oct_digits = format!("1{}", "0".repeat(digits - 1));
        let oct = Number::big(false, Radix::Oct, &oct_digits);
        let dec = Number::big(false, Radix::Dec, &to_decimal(&oct_digits, 8));
        let hex_digits = oct.as_big().unwrap().digits.to_string();
        let hex = Number::big(false, Radix::Hex, &hex_digits);
        assert_eq!(
            oct.as_big().unwrap().residue(),
            dec.as_big().unwrap().residue()
        );
        assert_eq!(h(&oct), h(&dec));
        for _ in 0..1000 {
            assert_ne!(oct, dec);
            assert_ne!(dec, hex);
            assert_eq!(oct, hex);
        }
    }

    /// Wide non-decimal integers are
    /// written as `0x` (linear); negative or base-60 ones convert to
    /// decimal only within the bound.
    #[test]
    fn core_writing_is_linear_or_refused() {
        let wide = "7".repeat(200_000);
        let oct = Number::big(false, Radix::Oct, &wide);
        let mut out = String::new();
        oct.write_core(&mut out, MAX_CONVERTED_DIGITS).unwrap();
        // 200,000 octal 7s = 600,000 one bits = 150,000 hex `f`s.
        assert_eq!(out, format!("0x{}", "f".repeat(150_000)));
        let neg = Number::big(true, Radix::Oct, &wide);
        let mut out = String::from("x");
        assert!(neg.write_core(&mut out, MAX_CONVERTED_DIGITS).is_err());
        assert_eq!(out, "x", "nothing written");
        let sx = Number::big(
            false,
            Radix::Sexagesimal,
            &format!("{}:00", "1".repeat(5000)),
        );
        assert!(
            sx.write_core(&mut String::new(), MAX_CONVERTED_DIGITS)
                .is_err()
        );
        // Within the bound: decimal.
        let neg = Number::big(
            true,
            Radix::Oct,
            "4000000000000000000000000000000000000000000",
        );
        let mut out = String::new();
        neg.write_core(&mut out, MAX_CONVERTED_DIGITS).unwrap();
        assert_eq!(out, "-340282366920938463463374607431768211456");
    }

    /// Octal/hex regrouping through an accumulator
    /// agrees with `u128` formatting for every width's padding case, and
    /// round-trips.
    #[test]
    fn regrouping_matches_u128_formatting() {
        let samples: Vec<u128> = (0..200u128)
            .chain((0..128).map(|k| 1u128 << k))
            .chain((1..128).map(|k| (1u128 << k) - 1))
            .chain([u128::MAX, 0o1234_5670_1234_5670, 0xdead_beef_0123_4567])
            .collect();
        for v in samples {
            let (oct, hex) = (format!("{v:o}"), format!("{v:x}"));
            let (oct, hex) = if v == 0 {
                (String::new(), String::new())
            } else {
                (oct, hex)
            };
            assert_eq!(oct_to_hex(&format!("{v:o}")), hex, "{v}");
            assert_eq!(hex_to_oct(&format!("{v:x}")), oct, "{v}");
            // Leading zeros in the input are dropped.
            assert_eq!(oct_to_hex(&format!("000{v:o}")), hex, "{v}");
            assert_eq!(hex_to_oct(&format!("000{v:x}")), oct, "{v}");
        }
        let wide = "1234567".repeat(10_000);
        assert_eq!(hex_to_oct(&oct_to_hex(&wide)), wide);
    }

    /// The bound counts **stored** digits. A 4,500-digit
    /// octal is stored as 3,375 hexadecimal digits (and 8^4499 has 4,064
    /// decimal digits), so it equals its decimal twin; its `Display` is
    /// still the octal text.
    #[test]
    fn bound_counts_stored_digits() {
        let oct_digits = format!("1{}", "0".repeat(4499));
        let oct = Number::big(false, Radix::Oct, &oct_digits);
        assert_eq!(oct.as_big().unwrap().digits.len(), 3375);
        let decimal = to_decimal(&oct_digits, 8);
        assert_eq!(decimal.len(), 4064);
        let dec = Number::big(false, Radix::Dec, &decimal);
        assert_eq!(oct, dec);
        assert_eq!(h(&oct), h(&dec));
        assert_eq!(oct.to_string(), format!("0o{oct_digits}"));
    }

    /// Past the bound, `to_core_string` still converts,
    /// and its decimal text is **not** `==` to the original once re-read.
    #[test]
    fn core_string_past_the_bound_is_not_equal_when_reread() {
        let wide = "7".repeat(5000);
        let neg = Number::big(true, Radix::Oct, &wide);
        let text = neg.to_core_string();
        let digits = text.strip_prefix('-').unwrap();
        assert!(digits.len() > MAX_CONVERTED_DIGITS);
        assert_eq!(digits, to_decimal(&wide, 8));
        let reread = Number::big(true, Radix::Dec, digits);
        assert_ne!(reread, neg);
        // Within the bound the re-read value is equal.
        let small = Number::big(true, Radix::Oct, &"7".repeat(100));
        let text = small.to_core_string();
        let reread = Number::big(true, Radix::Dec, text.strip_prefix('-').unwrap());
        assert_eq!(reread, small);
    }

    fn wide(neg: bool, text: &str, v: f64) -> Number {
        Number::out_of_range(neg, text, v).unwrap()
    }

    #[test]
    fn out_of_range_floats_compare_by_exact_value() {
        let a = wide(false, "1e400", f64::INFINITY);
        let b = wide(false, "2e400", f64::INFINITY);
        assert_ne!(a, b);
        assert_ne!(a, Number::from(f64::INFINITY));
        // The same value written differently is equal and hashes alike.
        let c = wide(false, "10e399", f64::INFINITY);
        let d = wide(false, "0.1e401", f64::INFINITY);
        let e = wide(false, "1000e397", f64::INFINITY);
        assert_eq!(a, c);
        assert_eq!(a, d);
        assert_eq!(a, e);
        assert_eq!(h(&a), h(&c));
        assert_eq!(h(&a), h(&e));
        assert_ne!(a, wide(true, "1e400", f64::NEG_INFINITY));
        let tiny = wide(false, "1e-400", 0.0);
        assert_ne!(tiny, wide(false, "2e-400", 0.0));
        assert_ne!(tiny, Number::from(0.0));
        assert_eq!(tiny.as_float(), Some(0.0));
        assert_eq!(a.as_f64(), Some(f64::INFINITY));
        assert!(a.is_float() && !a.is_integer());
    }

    #[test]
    fn out_of_range_float_with_a_huge_exponent() {
        let big = format!("1e{}", "9".repeat(100_000));
        let a = wide(false, &big, f64::INFINITY);
        let b = wide(false, &format!("10e{}8", "9".repeat(99_999)), f64::INFINITY);
        // 10e(99..98) is 1e(99..99) written with the mantissa shifted.
        assert_eq!(a, b);
        assert_ne!(
            a,
            wide(false, &format!("1e{}", "9".repeat(99_999)), f64::INFINITY)
        );
    }

    #[test]
    fn out_of_range_zero_is_not_a_wide_float() {
        assert!(Number::out_of_range(false, "0e999", 0.0).is_none());
        assert!(Number::out_of_range(false, "0.000", 0.0).is_none());
        assert!(Number::out_of_range(false, "x", 0.0).is_none());
    }

    #[test]
    fn out_of_range_text_is_a_float_with_a_signed_exponent() {
        assert_eq!(
            wide(false, "2.5e400", f64::INFINITY).to_string(),
            "2.5e+400"
        );
        assert_eq!(wide(true, "1e-400", -0.0).to_string(), "-1.0e-400");
    }

    #[test]
    fn decimal_exponent_arithmetic() {
        assert_eq!(add_small(false, "99", 1), "100");
        assert_eq!(add_small(false, "5", -7), "-2");
        assert_eq!(add_small(true, "5", 5), "0");
        assert_eq!(add_small(true, "100", 1), "-99");
        assert_eq!(add_small(true, "1", -1), "-2");
    }
}
