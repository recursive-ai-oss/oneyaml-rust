//! Markdown rendering of standard reports.

use core::fmt::Write as _;

use test_suite::standard::StandardReport;

fn notes(r: &StandardReport) -> String {
    let mut notes: Vec<String> = Vec::new();
    match r.profile {
        None => notes.push("no event API: valid column is n/a (see classification table)".into()),
        Some(p) => {
            let dropped = p.dropped();
            if !dropped.is_empty() {
                notes.push(format!("API lacks {}; ignored", dropped.join(", ")));
            }
        }
    }
    if r.panics() > 0 {
        notes.push(format!("{} panics (scored as failures)", r.panics()));
    }
    if r.truncations() > 0 {
        notes.push(format!(
            "{} runs cut off at the event cap (scored as failures)",
            r.truncations()
        ));
    }
    notes.join("; ")
}

/// The standard-criterion table: one row per report.
#[must_use]
pub fn standard_table(reports: &[StandardReport]) -> String {
    let mut out = String::from(
        "| Parser | Version | Valid | Error | Total | Error + prefix (vendored / corrected) | Notes |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for r in reports {
        let (vp, vt) = r.standard(false);
        let (ep, et) = r.standard(true);
        let total = vt.saturating_add(et);
        let (valid, sum, prefix) = if r.profile.is_some() {
            (
                format!("{vp}/{vt}"),
                format!("{}/{total}", vp.saturating_add(ep)),
                format!("{} / {} of {et}", r.prefix(false), r.prefix(true)),
            )
        } else {
            ("n/a".into(), "n/a".into(), "n/a".into())
        };
        let _ = writeln!(
            out,
            "| {} | {} | {valid} | {ep}/{et} | {sum} | {prefix} | {} |",
            r.name,
            r.version,
            notes(r)
        );
    }
    out
}

/// Classification (accept valid / reject invalid) and round trip, which every
/// oracle supports.
#[must_use]
pub fn classification_table(reports: &[StandardReport]) -> String {
    let mut out = String::from(
        "| Parser | Accepts valid | Rejects invalid | Both | Round trip (valid) |\n\
         |---|---|---|---|---|\n",
    );
    for r in reports {
        let (vt, et) = (r.standard(false).1, r.standard(true).1);
        let (a, j) = (r.classified(false), r.classified(true));
        let rt = if r.cases.iter().any(|c| c.round_trip.is_some()) {
            format!("{}/{vt}", r.round_trips())
        } else {
            "not measured".into()
        };
        let _ = writeln!(
            out,
            "| {} | {a}/{vt} | {j}/{et} | {}/{} | {rt} |",
            r.name,
            a.saturating_add(j),
            vt.saturating_add(et)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use test_suite::standard::{CaseScore, Observation, Profile};

    fn report(profile: Option<Profile>) -> StandardReport {
        let score = |id: &str, expect_error, classified, events| CaseScore {
            id: id.into(),
            expect_error,
            panic: None,
            classified,
            events,
            prefix_vendored: None,
            prefix_corrected: None,
            round_trip: None,
            observation: Observation::default(),
        };
        StandardReport {
            name: "p",
            version: "1",
            profile,
            cases: vec![
                score("A", false, true, profile.map(|_| true)),
                score("B", true, true, None),
            ],
        }
    }

    #[test]
    fn tables() {
        let t = standard_table(&[report(Some(Profile::FULL)), report(None)]);
        assert!(
            t.contains("| p | 1 | 1/1 | 1/1 | 2/2 | 0 / 0 of 1 |  |\n"),
            "{t}"
        );
        assert!(
            t.contains("| p | 1 | n/a | 1/1 | n/a | n/a | no event API"),
            "{t}"
        );
        let c = classification_table(&[report(None)]);
        assert!(c.contains("| p | 1/1 | 1/1 | 2/2 | not measured |"), "{c}");
    }
}
