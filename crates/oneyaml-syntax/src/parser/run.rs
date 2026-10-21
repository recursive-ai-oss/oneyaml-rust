//! Byte-run scanners: find where a run of ordinary bytes ends, sixteen bytes
//! at a time.
//!
//! Scalar text is mostly ordinary bytes with a rare special one (a quote, a
//! backslash, a `:`, a line break). Looking at each `char` of such text costs
//! many instructions a byte; these scanners test a whole machine word at once
//! with the carry-free "has a byte matching" bit tricks, so a long run costs a
//! fraction of an instruction a byte.
//!
//! # How a scanner is built
//!
//! Each public function takes the bytes, a start offset, and returns the
//! offset of the first *stop byte* at or after it (or the length). The word
//! test ([`skip_clean`]) only ever answers "this block certainly holds no
//! stop byte" or "it might"; it never reports a false negative. For a block
//! that might, the lowest flagged byte is a real match of one of the tests
//! (a borrow or carry only sets bits above a real match), so its position is
//! read straight off the word; a byte table then confirms it is a stop byte
//! (the test for "below `0x20`" also flags a tab, which is not one), and if
//! not the scan goes on after it. The result is exactly that of a plain
//! `position` loop; the word test only decides how fast the clean part is
//! passed. The unit tests check every scanner against that plain loop.
//!
//! # Cost model
//!
//! For a run of `n` bytes before the stop byte: about `n / 16` block tests
//! (a dozen instructions each, no branch on the data inside a block), plus a
//! constant for the block that holds the stop byte; a run shorter than eight
//! bytes in a scanner that starts with a byte-wise head costs a table lookup
//! a byte instead. No allocation, no recursion, no `unsafe`, no indexing:
//! every read is bounds-checked through `get`. Offsets past the end give the
//! length.

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

/// A word with a high bit in each byte of `w` that is below `n` (`n` ≤ 128).
/// Zero when there is none; set bits above the first real match may be false.
#[inline(always)]
const fn below(w: u64, n: u8) -> u64 {
    w.wrapping_sub(ONES.wrapping_mul(n as u64)) & !w & HIGH
}

/// A word with a high bit in each byte of `w` that equals `b`. Zero when there
/// is none; set bits above the first real match may be false.
#[inline(always)]
const fn equal(w: u64, b: u8) -> u64 {
    let x = w ^ ONES.wrapping_mul(b as u64);
    x.wrapping_sub(ONES) & !x & HIGH
}

/// A word with a high bit in each byte of `w` that is above `n` (`n` ≤ 127).
/// Zero when there is none; set bits above the first real match may be false.
#[inline(always)]
const fn above(w: u64, n: u8) -> u64 {
    (w.wrapping_add(ONES.wrapping_mul(127_u64.wrapping_sub(n as u64))) | w) & HIGH
}

#[inline(always)]
fn word(bytes: &[u8]) -> u64 {
    <[u8; 8]>::try_from(bytes).map_or(0, u64::from_le_bytes)
}

/// The first offset at or after `from` where a 16-byte block may hold a stop
/// byte (`flags` is nonzero for one of its two words), or the offset where
/// fewer than 16 bytes remain.
#[inline(always)]
fn skip_clean(bytes: &[u8], from: usize, flags: impl Fn(u64) -> u64) -> usize {
    let Some(rest) = bytes.get(from..) else {
        return bytes.len();
    };
    let mut at = 0usize;
    for block in rest.chunks_exact(16) {
        let (a, b) = block.split_at(8);
        if (flags(word(a)) | flags(word(b))) != 0 {
            break;
        }
        at = at.saturating_add(16);
    }
    from.saturating_add(at)
}

/// Byte classes, one table lookup each (see [`CLASS`]).
const BREAK: u8 = 1;
/// A control character other than a tab (a line break among them).
const CONTROL: u8 = 2;
const SINGLE_QUOTE: u8 = 4;
const DOUBLE_QUOTE: u8 = 8;
const BACKSLASH: u8 = 16;
/// [`plain_ordinary`] outside flow context.
const PLAIN: u8 = 32;
/// [`plain_ordinary`] in flow context.
const PLAIN_FLOW: u8 = 64;
/// Not printable ASCII or a tab: a control character, DEL, or a byte of a
/// multi-byte character (a line break is one).
const ODD: u8 = 128;

const fn classify(b: u8) -> u8 {
    let mut c = 0;
    if b == b'\n' || b == b'\r' {
        c |= BREAK;
    }
    if b < 0x20 && b != b'\t' {
        c |= CONTROL;
    }
    if b == b'\'' {
        c |= SINGLE_QUOTE;
    }
    if b == b'"' {
        c |= DOUBLE_QUOTE;
    }
    if b == b'\\' {
        c |= BACKSLASH;
    }
    if (b < 0x20 && b != b'\t') || b >= 0x7f {
        c |= ODD;
    }
    if matches!(b, 0x20..=0x7e) && b != b':' && b != b'#' {
        c |= PLAIN;
        if !matches!(b, b',' | b'[' | b']' | b'{' | b'}') {
            c |= PLAIN_FLOW;
        }
    }
    c
}

/// The class bits of every byte value, written out so that building it needs
/// neither indexing nor arithmetic.
#[rustfmt::skip]
static CLASS: [u8; 256] = [
    classify(0), classify(1), classify(2), classify(3), classify(4), classify(5), classify(6), classify(7), classify(8), classify(9), classify(10), classify(11), classify(12), classify(13), classify(14), classify(15),
    classify(16), classify(17), classify(18), classify(19), classify(20), classify(21), classify(22), classify(23), classify(24), classify(25), classify(26), classify(27), classify(28), classify(29), classify(30), classify(31),
    classify(32), classify(33), classify(34), classify(35), classify(36), classify(37), classify(38), classify(39), classify(40), classify(41), classify(42), classify(43), classify(44), classify(45), classify(46), classify(47),
    classify(48), classify(49), classify(50), classify(51), classify(52), classify(53), classify(54), classify(55), classify(56), classify(57), classify(58), classify(59), classify(60), classify(61), classify(62), classify(63),
    classify(64), classify(65), classify(66), classify(67), classify(68), classify(69), classify(70), classify(71), classify(72), classify(73), classify(74), classify(75), classify(76), classify(77), classify(78), classify(79),
    classify(80), classify(81), classify(82), classify(83), classify(84), classify(85), classify(86), classify(87), classify(88), classify(89), classify(90), classify(91), classify(92), classify(93), classify(94), classify(95),
    classify(96), classify(97), classify(98), classify(99), classify(100), classify(101), classify(102), classify(103), classify(104), classify(105), classify(106), classify(107), classify(108), classify(109), classify(110), classify(111),
    classify(112), classify(113), classify(114), classify(115), classify(116), classify(117), classify(118), classify(119), classify(120), classify(121), classify(122), classify(123), classify(124), classify(125), classify(126), classify(127),
    classify(128), classify(129), classify(130), classify(131), classify(132), classify(133), classify(134), classify(135), classify(136), classify(137), classify(138), classify(139), classify(140), classify(141), classify(142), classify(143),
    classify(144), classify(145), classify(146), classify(147), classify(148), classify(149), classify(150), classify(151), classify(152), classify(153), classify(154), classify(155), classify(156), classify(157), classify(158), classify(159),
    classify(160), classify(161), classify(162), classify(163), classify(164), classify(165), classify(166), classify(167), classify(168), classify(169), classify(170), classify(171), classify(172), classify(173), classify(174), classify(175),
    classify(176), classify(177), classify(178), classify(179), classify(180), classify(181), classify(182), classify(183), classify(184), classify(185), classify(186), classify(187), classify(188), classify(189), classify(190), classify(191),
    classify(192), classify(193), classify(194), classify(195), classify(196), classify(197), classify(198), classify(199), classify(200), classify(201), classify(202), classify(203), classify(204), classify(205), classify(206), classify(207),
    classify(208), classify(209), classify(210), classify(211), classify(212), classify(213), classify(214), classify(215), classify(216), classify(217), classify(218), classify(219), classify(220), classify(221), classify(222), classify(223),
    classify(224), classify(225), classify(226), classify(227), classify(228), classify(229), classify(230), classify(231), classify(232), classify(233), classify(234), classify(235), classify(236), classify(237), classify(238), classify(239),
    classify(240), classify(241), classify(242), classify(243), classify(244), classify(245), classify(246), classify(247), classify(248), classify(249), classify(250), classify(251), classify(252), classify(253), classify(254), classify(255),
];

/// The class bits of `b`.
#[inline(always)]
fn class(b: u8) -> u8 {
    CLASS.get(usize::from(b)).copied().unwrap_or(0)
}

/// Bytes looked at one by one before the block test takes over, for scanners
/// whose runs are often short (a quoted scalar): a short run never pays for
/// the block setup.
const HEAD: usize = 8;

/// The first offset at or after `from` whose byte has a class bit in `mask`,
/// or the length. `flags` must have a bit set for every word that holds such a
/// byte, and its lowest set bit must belong to a byte one of its patterns
/// really matches (true of the tests above: a borrow or carry only ever sets
/// bits above a real match). The class table then decides whether that byte
/// is a stop byte (a tab matches the control test but is not one).
#[inline(always)]
fn run_end(bytes: &[u8], from: usize, head: usize, mask: u8, flags: impl Fn(u64) -> u64) -> usize {
    let Some(rest) = bytes.get(from..) else {
        return bytes.len();
    };
    let head = rest.len().min(head);
    if let Some(k) = rest.iter().take(head).position(|b| class(*b) & mask != 0) {
        return from.saturating_add(k);
    }
    let mut at = from.saturating_add(head);
    'blocks: loop {
        let Some(rest) = bytes.get(at..) else {
            return bytes.len();
        };
        let mut off = 0usize;
        for block in rest.chunks_exact(16) {
            let (a, b) = block.split_at(8);
            let (fa, fb) = (flags(word(a)), flags(word(b)));
            if (fa | fb) != 0 {
                // The first flagged byte of the block.
                let k = if fa != 0 {
                    fa.trailing_zeros()
                } else {
                    fb.trailing_zeros().saturating_add(64)
                } / 8;
                let p = at.saturating_add(off).saturating_add(k as usize);
                if bytes.get(p).is_some_and(|b| class(*b) & mask != 0) {
                    return p;
                }
                // Not a stop byte (a tab): go on after it.
                at = p.saturating_add(1);
                continue 'blocks;
            }
            off = off.saturating_add(16);
        }
        // Fewer than 16 bytes remain: one by one.
        let tail = at.saturating_add(off);
        return bytes
            .get(tail..)
            .and_then(|t| t.iter().position(|b| class(*b) & mask != 0))
            .map_or(bytes.len(), |k| tail.saturating_add(k));
    }
}

/// The offset of the first line break (`\n` or `\r`) at or after `from`, or the
/// length: a faster `bytes[from..].position(|b| b == b'\n' || b == b'\r')`.
#[must_use]
pub(super) fn find_break(bytes: &[u8], from: usize) -> usize {
    // One test for every byte below `0x0e` (both breaks, and the tab and a few
    // controls, which the class table turns away).
    run_end(bytes, from, 0, BREAK, |w| below(w, 0x0e))
}

/// [`find_break`] that also says whether every byte before the break is
/// printable ASCII or a tab: the end of a comment line, and whether its text
/// can hold a character that needs a closer look (a control character, a
/// byte order mark, any non-ASCII character). One pass for both; when the
/// text is not clean, the end is found by a second scan from the first odd
/// byte.
#[must_use]
pub(super) fn line_end_checked(bytes: &[u8], from: usize) -> (usize, bool) {
    let odd = run_end(bytes, from, 0, ODD, |w| below(w, 0x20) | above(w, 0x7e));
    match bytes.get(odd) {
        // A break ends the line with nothing odd before it.
        None | Some(b'\n' | b'\r') => (odd, true),
        Some(_) => (find_break(bytes, odd), false),
    }
}

/// The end of the run of bytes a quoted scalar copies as they are: the first
/// byte at or after `from` that is a control character other than a tab (a
/// line break among them), the `quote`, or, in a double-quoted scalar, a
/// backslash. Bytes of multi-byte characters are all at least `0x80`, so they
/// are part of the run. `quote` is `"` when `double`, else `'`.
#[must_use]
pub(super) fn quoted_run_end(bytes: &[u8], from: usize, quote: u8, double: bool) -> usize {
    if double {
        run_end(bytes, from, HEAD, CONTROL | DOUBLE_QUOTE | BACKSLASH, |w| {
            below(w, 0x20) | equal(w, quote) | equal(w, b'\\')
        })
    } else {
        run_end(bytes, from, HEAD, CONTROL | SINGLE_QUOTE, |w| {
            below(w, 0x20) | equal(w, quote)
        })
    }
}

/// A byte that continues a plain scalar line without a closer look: printable
/// ASCII or a space, except `:` and `#` (which depend on their neighbours) and,
/// in flow context, a flow indicator.
#[inline(always)]
#[must_use]
pub(super) fn plain_ordinary(b: u8, flow: bool) -> bool {
    class(b) & if flow { PLAIN_FLOW } else { PLAIN } != 0
}

/// The flags of a word for [`plain_stop`]: a bit in each byte that is not
/// [`plain_ordinary`].
#[inline(always)]
const fn not_plain(w: u64, flow: bool) -> u64 {
    let f = below(w, 0x20) | above(w, 0x7e) | equal(w, b':') | equal(w, b'#');
    if flow {
        f | equal(w, b',') | equal(w, b'[') | equal(w, b']') | equal(w, b'{') | equal(w, b'}')
    } else {
        f
    }
}

/// The first offset at or after `from` whose byte is not [`plain_ordinary`],
/// or the length: where a plain scalar line needs a closer look.
///
/// The first word decides a short scalar at once (the lowest flagged byte of
/// a word is exact); long text goes on sixteen bytes at a time.
#[must_use]
pub(super) fn plain_stop(bytes: &[u8], from: usize, flow: bool) -> usize {
    let mut at = from;
    loop {
        let Some(rest) = bytes.get(at..) else {
            return bytes.len();
        };
        let Some(first) = rest.first_chunk::<8>() else {
            // Fewer than eight bytes remain.
            return rest
                .iter()
                .position(|b| !plain_ordinary(*b, flow))
                .map_or(bytes.len(), |k| at.saturating_add(k));
        };
        let f = not_plain(u64::from_le_bytes(*first), flow);
        if f != 0 {
            return at.saturating_add((f.trailing_zeros() / 8) as usize);
        }
        at = skip_clean(bytes, at.saturating_add(8), |w| not_plain(w, flow));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    /// A tiny deterministic generator: no timing, no global state.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
    }

    fn plain_find_break(b: &[u8], from: usize) -> usize {
        b.get(from..)
            .and_then(|r| r.iter().position(|x| matches!(x, b'\n' | b'\r')))
            .map_or(b.len(), |k| from + k)
    }

    fn plain_quoted(b: &[u8], from: usize, quote: u8, double: bool) -> usize {
        b.get(from..)
            .and_then(|r| {
                r.iter().position(|x| {
                    (*x < 0x20 && *x != b'\t') || *x == quote || (double && *x == b'\\')
                })
            })
            .map_or(b.len(), |k| from + k)
    }

    /// The ordinary-byte test the plain scalar scanner relies on.
    fn is_plain_ordinary(x: u8, flow: bool) -> bool {
        (0x20..=0x7e).contains(&x)
            && x != b':'
            && x != b'#'
            && !(flow && matches!(x, b',' | b'[' | b']' | b'{' | b'}'))
    }

    fn corpus(rng: &mut Lcg, len: usize) -> Vec<u8> {
        // Weighted toward specials, with every byte value reachable, so the
        // bit tricks meet each class next to each other and at every word
        // position.
        const SPECIAL: &[u8] = b"\n\r\t \"'\\:#,[]{}\x00\x1f\x20\x7e\x7f\x80\xff\xc3\xa9";
        (0..len)
            .map(|_| {
                let r = rng.next();
                if r % 4 == 0 {
                    SPECIAL[(r >> 8) as usize % SPECIAL.len()]
                } else if r % 7 == 0 {
                    (r >> 8) as u8
                } else {
                    b'a' + (r >> 8) as u8 % 26
                }
            })
            .collect()
    }

    #[test]
    fn single_special_byte_at_every_offset() {
        // One stop byte in a field of ordinary ones, at every position of
        // lengths around the 8- and 16-byte block boundaries.
        for len in 0..70usize {
            for at in 0..len {
                for special in [b'\n', b'\r', b'"', b'\'', b'\\', 0x01_u8, b':', b'#'] {
                    let mut v = vec![b'x'; len];
                    v[at] = special;
                    for from in [0, 1, 7, 8, 9, 15, 16, 17, 33]
                        .into_iter()
                        .filter(|f| *f <= len)
                    {
                        assert_eq!(
                            find_break(&v, from),
                            plain_find_break(&v, from),
                            "{v:?} {from}"
                        );
                        for (quote, double) in [(b'"', true), (b'\'', false)] {
                            assert_eq!(
                                quoted_run_end(&v, from, quote, double),
                                plain_quoted(&v, from, quote, double),
                                "{v:?} {from} {quote}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn no_special_byte_gives_the_length() {
        for len in 0..70usize {
            let v = vec![b'x'; len];
            assert_eq!(find_break(&v, 0), len);
            assert_eq!(quoted_run_end(&v, 0, b'"', true), len);
            assert_eq!(quoted_run_end(&v, 0, b'\'', false), len);
            // Past the end is the length, never a panic.
            assert_eq!(find_break(&v, len + 5), len);
            assert_eq!(quoted_run_end(&v, len + 5, b'"', true), len);
        }
    }

    #[test]
    fn tab_and_multibyte_text_stay_in_a_quoted_run() {
        let text = "tab\there caf\u{e9} \u{1f600} \u{2028} end\"";
        let b = text.as_bytes();
        assert_eq!(quoted_run_end(b, 0, b'"', true), b.len() - 1);
        assert_eq!(quoted_run_end(b, 0, b'\'', false), b.len());
        assert_eq!(
            quoted_run_end(b, 0, b'"', true),
            plain_quoted(b, 0, b'"', true)
        );
    }

    #[test]
    fn random_text_matches_the_plain_scans() {
        let mut rng = Lcg(0x5eed);
        for round in 0..4000 {
            let len = (rng.next() % 90) as usize;
            let v = corpus(&mut rng, len);
            let from = if len == 0 {
                0
            } else {
                (rng.next() as usize) % (len + 2)
            };
            assert_eq!(
                find_break(&v, from),
                plain_find_break(&v, from),
                "round {round}"
            );
            for (quote, double) in [(b'"', true), (b'\'', false)] {
                assert_eq!(
                    quoted_run_end(&v, from, quote, double),
                    plain_quoted(&v, from, quote, double),
                    "round {round}"
                );
            }
        }
    }

    #[test]
    fn plain_stop_is_the_first_byte_needing_a_look() {
        let mut rng = Lcg(77);
        for round in 0..6000 {
            let len = (rng.next() % 90) as usize;
            let v = corpus(&mut rng, len);
            let from = if len == 0 {
                0
            } else {
                (rng.next() as usize) % (len + 2)
            };
            for flow in [false, true] {
                let want = v
                    .iter()
                    .enumerate()
                    .skip(from)
                    .find(|(_, b)| !is_plain_ordinary(**b, flow))
                    .map_or(v.len(), |(k, _)| k);
                assert_eq!(
                    plain_stop(&v, from, flow),
                    want,
                    "round {round} flow {flow}: {v:?} {from}"
                );
            }
        }
    }

    #[test]
    fn plain_stop_at_every_offset_of_ordinary_text() {
        for len in 0..70usize {
            for flow in [false, true] {
                let ordinary = vec![b'x'; len];
                assert_eq!(plain_stop(&ordinary, 0, flow), len);
                for at in 0..len {
                    for special in [b':', b'#', b'\t', b'\n', 0x7f_u8, 0xc3, b'[', b','] {
                        let mut v = ordinary.clone();
                        v[at] = special;
                        let want = if flow || !matches!(special, b'[' | b',') {
                            at
                        } else {
                            len
                        };
                        assert_eq!(plain_stop(&v, 0, flow), want, "{v:?} flow {flow}");
                    }
                }
            }
        }
    }

    #[test]
    fn plain_ordinary_agrees_with_the_character_classes() {
        use super::super::scan::{is_flow_indicator, is_ns_char};
        for b in 0..=255u8 {
            for flow in [false, true] {
                let expected = b < 0x80
                    && (b == b' '
                        || (is_ns_char(char::from(b))
                            && b != b':'
                            && b != b'#'
                            && !(flow && is_flow_indicator(char::from(b)))));
                assert_eq!(plain_ordinary(b, flow), expected, "{b:#x} flow {flow}");
                assert_eq!(is_plain_ordinary(b, flow), expected, "{b:#x} flow {flow}");
            }
        }
    }
}
