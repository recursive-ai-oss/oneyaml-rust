//! The one documented exception to "an accepted edit equals a re-parse of
//! its text" (the `-syntax` tree docs, *Recovered input*, "Known exception";
//! deferred post-1.0): under `lenient_inline_colons`, a document
//! holding a failed directive block (`Y0001`) whose diagnostics end in a
//! `Y0028` warning can hide an unclosed quote's read-on past a byte order
//! mark from the validator, which decides from the document's last
//! diagnostic. The edit oracles excuse exactly that class and
//! count it; anything else that differs from its re-parse is a finding.

use std::sync::atomic::{AtomicUsize, Ordering};

use oneyaml_core::Span;
use oneyaml_core::code::{
    DIRECTIVE_WITHOUT_DOCUMENT, LENIENT_INLINE_COLON, UNTERMINATED_QUOTED_SCALAR,
};
use oneyaml_syntax::tree::{SyntaxKind, SyntaxTree};

/// Mismatches excused as the known exception, by this copy of the module.
static HITS: AtomicUsize = AtomicUsize::new(0);

/// Excused mismatches so far (this copy of the module). (Read by the
/// fuzz crate's replay; the `bench` test crates only excuse.)
#[allow(dead_code)]
pub(crate) fn hits() -> usize {
    HITS.load(Ordering::Relaxed)
}

/// Whether `tree` shows the known exception's shape: lenient settings, a
/// byte order mark in the text, and a stream-level document holding a
/// `Y0001` whose last diagnostic is a `Y0028` warning.
fn shape(tree: &SyntaxTree) -> bool {
    if !tree.settings().get_lenient_inline_colons() || !tree.emit().contains('\u{feff}') {
        return false;
    }
    tree.root()
        .children()
        .filter(|n| n.kind() == SyntaxKind::Document)
        .any(|d| {
            let codes: Vec<_> = tree.diagnostics_within(d).map(|(_, x)| x.code).collect();
            codes.contains(&DIRECTIVE_WITHOUT_DOCUMENT)
                && codes.last() == Some(&LENIENT_INLINE_COLON)
        })
}

/// Whether the mismatch between the two
/// trees involves a quoted scalar or a byte order mark, as the exception
/// does (a quote read on past a byte order mark): either their tokens
/// (kind and text, in order) first differ at a quoted scalar or a byte
/// order mark on either side, or, with the same tokens, their first
/// differing diagnostic (on either side) is an unclosed quote, starts at a
/// quote or touches a quoted scalar or a byte order mark. A mismatch
/// elsewhere is not it.
fn quote_or_bom_mismatch(edited: &SyntaxTree, again: &SyntaxTree) -> bool {
    let special = |k: SyntaxKind| {
        matches!(
            k,
            SyntaxKind::SingleQuotedScalar
                | SyntaxKind::DoubleQuotedScalar
                | SyntaxKind::ByteOrderMark
        )
    };
    let tokens = |t: &SyntaxTree| -> Vec<(SyntaxKind, Span, String)> {
        t.root()
            .descendants()
            .filter(|n| n.is_token())
            .map(|n| (n.kind(), n.span(), n.text().into_owned()))
            .collect()
    };
    let (a, b) = (tokens(edited), tokens(again));
    let key = |x: Option<&(SyntaxKind, Span, String)>| x.map(|(k, _, t)| (*k, t.clone()));
    if let Some(i) = (0..a.len().max(b.len())).find(|&i| key(a.get(i)) != key(b.get(i))) {
        return [a.get(i), b.get(i)]
            .into_iter()
            .flatten()
            .any(|(k, _, _)| special(*k));
    }
    // The same tokens (so the same text): the first differing diagnostic.
    let text = edited.emit();
    let (da, db): (Vec<_>, Vec<_>) = (
        edited.diagnostics().collect(),
        again.diagnostics().collect(),
    );
    let Some(i) = (0..da.len().max(db.len())).find(|&i| da.get(i) != db.get(i)) else {
        return false;
    };
    [da.get(i), db.get(i)].into_iter().flatten().any(|d| {
        let s = d.primary.span;
        // An unclosed quote is reported where the quote is cut off, not at
        // the quote.
        d.code == UNTERMINATED_QUOTED_SCALAR
            || text
                .get(s.start..)
                .is_some_and(|t| t.starts_with(['\'', '"']))
            || a.iter()
                .any(|(k, t, _)| special(*k) && t.start <= s.end && s.start <= t.end)
    })
}

/// Whether a mismatch between the edited tree `edited` and `again`, its
/// re-parse, is the known exception (either side of the comparison
/// shows the shape: the validator decided on the edited tree, the
/// re-parse reads the quote on), and the mismatch involves a quoted
/// scalar or a byte order mark. Counted, and logged once per
/// hit on stderr (`KNOWN-EXCEPTION`) so that campaign logs can be
/// tallied.
pub(crate) fn excused(edited: &SyntaxTree, again: &SyntaxTree) -> bool {
    let hit = (shape(edited) || shape(again)) && quote_or_bom_mismatch(edited, again);
    if hit {
        HITS.fetch_add(1, Ordering::Relaxed);
        eprintln!("KNOWN-EXCEPTION lenient-directive-bom");
    }
    hit
}
