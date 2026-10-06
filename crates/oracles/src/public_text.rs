//! Reader-facing text checks for everything the generators write.
//!
//! `bench update`, `bench readme` and `oracles --readme` write into the
//! public READMEs, `docs/*.md` and the benchmark charts. Those are read by
//! people who only have the published repository, so generated text must
//! not carry the project's internal process identifiers: decision and
//! open-issue IDs, references to the private planning documents,
//! work-unit and phase numbers, or review jargon (the exact forms are
//! listed on [`internal_reference`]).
//! [`internal_reference`] finds them; the generators refuse to write text
//! that has one, and `bench`'s `tests/public_text.rs` checks the committed
//! generated regions and the generators' own string literals.

/// The first internal identifier in `text`, as it appears there, or `None`
/// when `text` reads as plain user-facing documentation.
///
/// Matching is word-bounded (no ASCII letter or digit right before it):
///
/// * decision IDs `Q4.4`, `Q10.2` and the family form `Q4.x`; a bare `Q4`
///   (a calendar quarter) and the heading slug `q44` pass;
/// * `NN-3` and open-issue IDs `OI-1`, with `-` or any Unicode hyphen or
///   minus (`NN‑3` with U+2011, `OI–1`);
/// * the upper-case `DESIGN` and `RULES` (whole words) and `PLAN.md`;
/// * "unit", "units" or "phase" (any case) followed by a digit, directly
///   (`unit49b`), after a space (`unit 49b`, also no-break and thin spaces)
///   or after a hyphen (`unit-49b`); `unit tests` and `community 5` pass;
/// * review jargon as whole words: architect, reviewer, ruling, waiver
///   (plural allowed).
#[must_use]
pub fn internal_reference(text: &str) -> Option<String> {
    let b = text.as_bytes();
    (0..b.len()).find_map(|i| match_at(b, i).and_then(|end| text.get(i..end).map(str::to_owned)))
}

/// Hyphens and minus signs: `-`, U+2010–U+2014, U+2212, U+FE63, U+FF0D.
const DASHES: [&str; 9] = [
    "-", "\u{2010}", "\u{2011}", "\u{2012}", "\u{2013}", "\u{2014}", "\u{2212}", "\u{fe63}",
    "\u{ff0d}",
];

/// Spaces between a word and its number: space, tab, no-break (U+00A0),
/// thin (U+2009) and narrow no-break (U+202F) space.
const SPACES: [&str; 5] = [" ", "\t", "\u{a0}", "\u{2009}", "\u{202f}"];

/// Byte `i` of `b`, or `0` past the end.
fn at(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}

/// The length of the first of `set` at byte `i` of `b`, if one is there.
fn one_of(b: &[u8], i: usize, set: &[&str]) -> Option<usize> {
    let rest = b.get(i..)?;
    set.iter()
        .find(|s| rest.starts_with(s.as_bytes()))
        .map(|s| s.len())
}

/// Whether a word may start at `i` (no letter or digit before it).
fn word_start(b: &[u8], i: usize) -> bool {
    i.checked_sub(1)
        .is_none_or(|p| !at(b, p).is_ascii_alphanumeric())
}

/// Whether `word` (ASCII) is at `i`, ignoring case when `fold`.
fn word_at(b: &[u8], i: usize, word: &str, fold: bool) -> bool {
    let end = i.saturating_add(word.len());
    b.get(i..end).is_some_and(|s| {
        if fold {
            s.eq_ignore_ascii_case(word.as_bytes())
        } else {
            s == word.as_bytes()
        }
    })
}

/// `j` past an optional plural `s`/`S`.
fn plural(b: &[u8], j: usize) -> usize {
    if matches!(at(b, j), b's' | b'S') {
        j.saturating_add(1)
    } else {
        j
    }
}

/// The end of an internal identifier starting at byte `i`, if one does.
fn match_at(b: &[u8], i: usize) -> Option<usize> {
    if !word_start(b, i) {
        return None;
    }
    let step = |k: usize| i.saturating_add(k);
    // Decision IDs: `Q1.2`, `Q10.2`, `Q4.x`.
    if at(b, i) == b'Q' && at(b, step(1)).is_ascii_digit() {
        let mut j = step(1);
        while at(b, j).is_ascii_digit() {
            j = j.saturating_add(1);
        }
        let after = j.saturating_add(1);
        if at(b, j) == b'.'
            && (at(b, after).is_ascii_digit() || matches!(at(b, after), b'x' | b'X'))
        {
            return Some(after.saturating_add(1));
        }
    }
    // `NN-4`, `OI-1`, with any hyphen.
    for prefix in ["NN", "OI"] {
        if word_at(b, i, prefix, false)
            && let Some(d) = one_of(b, step(2), &DASHES)
        {
            let digit = step(2).saturating_add(d);
            if at(b, digit).is_ascii_digit() {
                return Some(digit.saturating_add(1));
            }
        }
    }
    // The private planning documents.
    for doc in ["DESIGN", "RULES"] {
        if word_at(b, i, doc, false) && !at(b, step(doc.len())).is_ascii_alphanumeric() {
            return Some(step(doc.len()));
        }
    }
    if word_at(b, i, "PLAN.md", false) {
        return Some(step(7));
    }
    // Work units and phases: `unit 49b`, `Units 41/41b`, `unit-49b`,
    // `unit49b`, `Phase 6`.
    for word in ["unit", "phase"] {
        if word_at(b, i, word, true) {
            let j = plural(b, step(word.len()));
            let sep = one_of(b, j, &SPACES)
                .or_else(|| one_of(b, j, &DASHES))
                .unwrap_or(0);
            let digit = j.saturating_add(sep);
            if at(b, digit).is_ascii_digit() {
                return Some(digit.saturating_add(1));
            }
        }
    }
    // Review jargon, whole words (plural allowed).
    for word in ["architect", "reviewer", "ruling", "waiver"] {
        if word_at(b, i, word, true) {
            let j = plural(b, step(word.len()));
            if !at(b, j).is_ascii_alphanumeric() {
                return Some(j);
            }
        }
    }
    None
}

/// `Err` naming the identifier when `text` (written to `what`) has one.
///
/// # Errors
/// The first internal identifier in `text`, with where it would go.
pub fn check(what: &str, text: &str) -> Result<(), String> {
    match internal_reference(text) {
        None => Ok(()),
        Some(found) => Err(format!(
            "{what} would contain {found:?}, an internal process identifier readers of the \
             published docs cannot resolve; write it in plain words"
        )),
    }
}

/// Every generated region of a markdown document: `(marker name, body)` for
/// each `<!-- NAME:begin -->` … `<!-- NAME:end -->` pair whose `NAME` is
/// `bench:…` or `oracles:…` (lower-case letters, digits and `-` after the
/// colon). Prose that merely mentions the marker form (`begin/end`) is not
/// a region.
#[must_use]
pub fn generated_regions(doc: &str) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    let mut rest = doc;
    while let Some(start) = rest.find("<!-- ") {
        let after = rest.get(start.saturating_add(5)..).unwrap_or_default();
        let Some(name) = after
            .split(':')
            .next()
            .filter(|t| matches!(*t, "bench" | "oracles"))
        else {
            rest = after;
            continue;
        };
        let tail = after
            .get(name.len().saturating_add(1)..)
            .unwrap_or_default();
        let id: String = tail
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .collect();
        let begin_rest = tail.get(id.len()..).unwrap_or_default();
        if id.is_empty() || !begin_rest.starts_with(":begin -->") {
            rest = after;
            continue;
        }
        let body_from = begin_rest.get(":begin -->".len()..).unwrap_or_default();
        let end = format!("<!-- {name}:{id}:end -->");
        match body_from.find(&end) {
            Some(e) => {
                out.push((
                    format!("{name}:{id}"),
                    body_from.get(..e).unwrap_or_default(),
                ));
                rest = body_from
                    .get(e.saturating_add(end.len())..)
                    .unwrap_or_default();
            }
            None => rest = body_from,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn finds_internal_identifiers() {
        for (text, found) in [
            ("target Q4.4 ≤ 1.25×", "Q4.4"),
            ("(Q1.2: correctness)", "Q1.2"),
            ("see NN-3", "NN-3"),
            ("waived (DESIGN Q4.4)", "DESIGN"),
            ("RULES: retained", "RULES"),
            ("see PLAN.md", "PLAN.md"),
            ("`4c6d70c` + unit 49b", "unit 4"),
            ("`a` + Units 41/41b", "Units 4"),
            ("#q44-target-audit-unit-49b", "unit-4"),
            ("Phase 6 exit", "Phase 6"),
            ("the architect's call", "architect"),
            ("Reviewers found", "Reviewers"),
            ("an owner ruling", "ruling"),
            ("the waiver's figures", "waiver"),
            // Open issues, the family form, wider decision numbers.
            ("lenient (OI-1)", "OI-1"),
            ("the Q4.x targets", "Q4.x"),
            ("Q10.2", "Q10.2"),
            // Unicode hyphens and spaces.
            ("NN\u{2011}3", "NN\u{2011}3"),
            ("OI\u{2013}2", "OI\u{2013}2"),
            ("NN\u{2212}1", "NN\u{2212}1"),
            ("unit\u{2011}49b", "unit\u{2011}4"),
            ("unit\u{a0}49b", "unit\u{a0}4"),
            // No separator at all.
            ("since unit49b", "unit4"),
            ("Units41/41b", "Units4"),
            ("phase6 exit", "phase6"),
        ] {
            assert_eq!(internal_reference(text).as_deref(), Some(found), "{text}");
        }
    }

    #[test]
    fn plain_text_passes() {
        for text in [
            "",
            "oneyaml ÷ fastest feature-complete",
            "target ≤ 1.25×",
            "no target (< 100 KB)",
            "`abc1234` + uncommitted changes",
            "only the benchmark harness and docs differed from it",
            "`a` + unit tests",
            "community 5",
            "SQ4.4 Q4 q4.4 Q4.y NNN-3 NN-x OI-x",
            "DESIGNS RULESET plan.md design rules",
            "architecture, reviewed, overruled",
            "a unit of 4 bytes, units: Ir/B, phased 2",
            "known loss on small documents; waived",
            // A calendar quarter, a plain OI word, a decimal after Q.
            "shipped in Q4 2026",
            "Q4. Next",
            "OIL-3 COIN-1 NNN\u{2011}3",
            "unitary 4, phased-6, units of 8",
        ] {
            assert_eq!(internal_reference(text), None, "{text}");
        }
        assert!(check("x", "plain").is_ok());
        assert!(
            check("the README", "Q4.4")
                .unwrap_err()
                .contains("the README")
        );
    }

    #[test]
    fn regions() {
        let doc = "a `<!-- bench:NAME:begin/end -->` b\n<!-- bench:readme:begin -->\nX\n\
                   <!-- bench:readme:end -->\n  <!-- oracles:conformance:begin -->\n  Y\n  \
                   <!-- oracles:conformance:end -->\n<!-- other:x:begin -->Z<!-- other:x:end -->\
                   <!-- bench:open:begin --> never closed";
        let r = generated_regions(doc);
        let names: Vec<&str> = r.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["bench:readme", "oracles:conformance"]);
        assert_eq!(r[0].1, "\nX\n");
        assert_eq!(r[1].1.trim(), "Y");
    }
}
