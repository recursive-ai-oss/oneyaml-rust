//! Diagnostic polish: labels, help, and fixes for the common mistakes
//! (`'it's'`, `"C:\path"`, reserved `@`/`` ` ``,
//! missing closing quotes). Pure functions of the source; the parser only
//! attaches their result to the diagnostic it reports anyway, so codes,
//! messages, spans, and events never depend on them.
//!
//! A fix is **machine-applicable** only when applying it makes the node
//! complete and valid on its own line and nothing after it can read it
//! differently: the rest of the line is white space, a comment, or (for a
//! key, or in flow context) the indicator that follows a node, and in
//! block context no more-indented line continues it. Otherwise it is
//! offered as maybe-incorrect. Every scan here stops at the end of the
//! diagnostic's line or at the first line that is not blank, so a
//! diagnostic costs O(its line + the blank lines after it).

use oneyaml_core::code::{
    INVALID_ESCAPE, INVALID_NODE_START, QUOTED_SCALAR_INDENTATION, UNTERMINATED_QUOTED_SCALAR,
};
use oneyaml_core::{Applicability, Code, Diagnostic, Label, Replacement, Span, Suggestion};

use super::head::quote_fix;
use super::scalar::escape_scan;
use super::scan::{break_len, byte, is_nb_json, line_end, skip_white};

/// What a polished diagnostic gains.
#[derive(Debug, Default)]
pub(super) struct Polish {
    labels: Vec<Label>,
    help: Option<&'static str>,
    fixes: Vec<Suggestion>,
}

impl Polish {
    /// Every fix as maybe-incorrect.
    fn downgrade(mut self) -> Self {
        for fix in &mut self.fixes {
            fix.applicability = Applicability::MaybeIncorrect;
        }
        self
    }

    /// Adds the labels, help (replacing a default), and fixes to `d`.
    pub(super) fn apply(self, d: Diagnostic) -> Diagnostic {
        let mut d = d;
        for label in self.labels {
            d = d.with_secondary(label);
        }
        if let Some(help) = self.help {
            d = d.with_help(help);
        }
        for fix in self.fixes {
            d = d.with_suggestion(fix);
        }
        d
    }
}

/// Where the failed node sits.
#[derive(Clone, Copy, Debug)]
pub(super) struct Place {
    /// The node may be (block context) or is followed by (flow context) a
    /// key's `:`; its text ends there.
    pub(super) key: bool,
    /// The node must be an implicit key (a mapping's key position): it is
    /// only fixed when its `:` follows.
    pub(super) key_only: bool,
    /// Indentation of the block parent (`None`: the document level).
    pub(super) parent: Option<usize>,
    /// Inside a flow collection.
    pub(super) flow: bool,
    /// Nothing else about the node is known to be wrong (e.g. its
    /// properties do not clash with earlier ones): only then can a fix be
    /// machine-applicable.
    pub(super) sound: bool,
}

fn how(safe: bool) -> Applicability {
    if safe {
        Applicability::MachineApplicable
    } else {
        Applicability::MaybeIncorrect
    }
}

/// Whether a more-indented line after the line holding `at` would continue
/// (or, after a complete node, break) the node in block context. Blank
/// lines are skipped; a comment line, a document marker, a less-indented
/// line, or the end ends it. A line indented with a tab is assumed to
/// continue (the conservative answer).
fn continues(src: &str, at: usize, place: Place) -> bool {
    if place.flow {
        return false;
    }
    let mut i = line_end(src, at);
    loop {
        let br = break_len(src, i);
        if br == 0 {
            return false; // the end of the input
        }
        let start = i.saturating_add(br);
        let content = src
            .get(start..)
            .map_or(0, |r| r.bytes().take_while(|b| *b == b' ').count());
        let first = start.saturating_add(content);
        match byte(src, first) {
            None => return false,
            Some(b'\n' | b'\r') => i = first,
            Some(b'\t') => {
                // White space only, or a tab-indented line.
                let after = skip_white(src, first);
                if matches!(byte(src, after), None | Some(b'\n' | b'\r')) {
                    i = after;
                } else {
                    return true;
                }
            }
            Some(b'#') => return false,
            Some(_) => {
                let marker = content == 0
                    && src
                        .get(start..)
                        .is_some_and(|r| r.starts_with("---") || r.starts_with("..."));
                return !marker && place.parent.is_none_or(|p| content > p);
            }
        }
    }
}

/// Whether what follows a complete node at `end` on its line leaves it as
/// it is: white space, then the end of the line, a comment, or (for a key
/// or in flow context) the indicator that may follow a node; a node that
/// must be a key needs its `:`.
fn clean_after(src: &str, end: usize, place: Place) -> bool {
    let i = skip_white(src, end);
    // A block key's `:` must be followed by white space or the line's end.
    let colon = byte(src, i) == Some(b':')
        && (place.flow
            || matches!(
                byte(src, i.saturating_add(1)),
                None | Some(b' ' | b'\t' | b'\n' | b'\r')
            ));
    if place.key_only {
        // A key needs its `:`.
        return colon;
    }
    match byte(src, i) {
        None | Some(b'\n' | b'\r') => true,
        Some(b'#') => i > end,
        Some(b':') => colon && place.key,
        Some(b',' | b']' | b'}') => place.flow,
        _ => false,
    }
}

/// Where a value starting at `from` ends on its line: before a comment
/// (`#` after white space) and trailing white space; for a key (`key`),
/// before a `:` followed by white space, the line's end, or (in flow
/// context) a flow indicator; in flow context, also before `,`, `[`, `]`,
/// `{`, `}`. Also returns the indicator that ended it, if one did (not the
/// line's end or a comment).
fn value_end(src: &str, from: usize, key: bool, flow: bool) -> (usize, Option<u8>) {
    let bytes = src.as_bytes();
    let line = line_end(src, from);
    let mut end = line;
    let mut stop = None;
    let mut prev_white = false;
    for (k, b) in bytes.get(from..line).unwrap_or(&[]).iter().enumerate() {
        let at = from.saturating_add(k);
        let next = bytes.get(at.saturating_add(1));
        let separated = next.is_none_or(|n| {
            matches!(n, b' ' | b'\t' | b'\n' | b'\r')
                || flow && matches!(n, b',' | b'[' | b']' | b'{' | b'}')
        });
        let colon = *b == b':' && key && separated;
        let bracket = flow && matches!(b, b',' | b'[' | b']' | b'{' | b'}');
        if *b == b'#' && prev_white {
            end = at;
            break;
        }
        if colon || bracket {
            end = at;
            stop = Some(*b);
            break;
        }
        prev_white = matches!(b, b' ' | b'\t');
    }
    let trimmed = src
        .get(from..end)
        .map_or(0, |t| t.trim_end_matches([' ', '\t']).len());
    (from.saturating_add(trimmed), stop)
}

/// `text` is a complete, valid double-quoted scalar on one line.
fn valid_double(text: &str) -> bool {
    let scan = escape_scan(text, 0);
    scan.end == Some(text.len())
        && scan.first.is_none()
        && text
            .chars()
            .all(|c| is_nb_json(c) && c != '\n' && c != '\r')
}

/// `text` is a complete, valid single-quoted scalar on one line: quoted,
/// every inner `'` doubled.
fn valid_single(text: &str) -> bool {
    let Some(inner) = text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) else {
        return false;
    };
    let mut quotes = 0usize;
    for c in inner.chars() {
        if c == '\'' {
            quotes = quotes.saturating_add(1);
        } else {
            if quotes % 2 == 1 {
                return false;
            }
            quotes = 0;
        }
        if !is_nb_json(c) || c == '\n' || c == '\r' {
            return false;
        }
    }
    quotes % 2 == 0
}

/// The text between the quotes of a scalar whose inner quotes were not
/// escaped: every lone `'` doubled (single quotes), or every `"` escaped
/// (double quotes; escapes already there are kept).
fn escape_quotes(inner: &str, double: bool) -> String {
    let mut out = String::with_capacity(inner.len().saturating_add(4));
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => {
                out.push_str("''");
                if chars.peek() == Some(&'\'') {
                    chars.next();
                }
            }
            '\\' if double => {
                out.push(c);
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '"' if double => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// A failed node head whose content starts at `at` (diagnostic polish).
pub(super) fn content(src: &str, code: Code, at: usize, place: Place) -> Option<Polish> {
    let polish = match (code, byte(src, at)) {
        (INVALID_ESCAPE, Some(b'"')) => Some(invalid_escape(src, at, place)),
        (INVALID_NODE_START, Some(c @ (b'@' | b'`'))) => Some(reserved(src, at, c, place)),
        (UNTERMINATED_QUOTED_SCALAR | QUOTED_SCALAR_INDENTATION, Some(q @ (b'"' | b'\''))) => {
            Some(unclosed_quote(src, code, at, q))
        }
        _ => None,
    };
    polish.map(|p| if place.sound { p } else { p.downgrade() })
}

/// A bad escape in the double-quoted scalar at `at`: a label on the first
/// one, fixes making the backslashes literal or switching to single
/// quotes.
fn invalid_escape(src: &str, at: usize, place: Place) -> Polish {
    let scan = escape_scan(src, at);
    let mut polish = Polish::default();
    if let Some(first) = scan.first {
        let text = src.get(first.start..first.end).unwrap_or("\\");
        polish
            .labels
            .push(Label::new(first, format!("`{text}` is not an escape")));
        if text == "\\'" {
            polish.help = Some("a `'` needs no escape inside double quotes; remove the `\\`");
        }
    }
    let Some(end) = scan.end else {
        // Unclosed on its line: the scalar may hold more bad escapes.
        if !scan.edits.is_empty() {
            polish.fixes.push(Suggestion::new(
                "make the backslashes literal",
                scan.edits,
                Applicability::MaybeIncorrect,
            ));
        }
        return polish;
    };
    let settled = clean_after(src, end, place) && !continues(src, end, place);
    // The scalar as the fix leaves it, checked like the parser would.
    let fixed = {
        let mut out = String::new();
        let mut from = at;
        for e in &scan.edits {
            out.push_str(src.get(from..e.span.start).unwrap_or(""));
            out.push_str(&e.text);
            from = e.span.end;
        }
        out.push_str(src.get(from..end).unwrap_or(""));
        out
    };
    let single = (!scan.valid && !scan.apostrophe)
        .then(|| src.get(at.saturating_add(1)..end.saturating_sub(1)))
        .flatten()
        .map(|inner| format!("'{inner}'"))
        .filter(|t| valid_single(t));
    if !scan.edits.is_empty() {
        polish.fixes.push(Suggestion::new(
            "make the backslashes literal",
            scan.edits,
            // A valid escape other than `\\`/`\"` may have been meant
            // literally too: then the value is not certain.
            how(settled && valid_double(&fixed) && !scan.ambiguous),
        ));
    }
    if let Some(text) = single {
        polish.fixes.push(Suggestion::new(
            "use single quotes, where `\\` is literal",
            vec![Replacement::new(Span::new(at, end), text)],
            Applicability::MaybeIncorrect,
        ));
    }
    polish
}

/// A reserved indicator (`@`, `` ` ``) starting a plain scalar: quote the
/// value.
fn reserved(src: &str, at: usize, c: u8, place: Place) -> Polish {
    let (end, stop) = value_end(src, at, place.key, place.flow);
    let text = src.get(at..end).unwrap_or("");
    let fix = quote_fix(src, at, end, Applicability::MaybeIncorrect);
    // Quoting keeps exactly this text. It is the whole value when nothing
    // continues it: in flow context an indicator ended it on this line; in
    // block context no more-indented line follows (a `: ` inside may mean
    // a mapping was meant).
    // A key's `:` must follow it on its line.
    // In flow context the value must end at `,`, `]`, `}` (or a key's
    // `:`) on this line: a `[` or `{` cannot follow a node.
    let ends_node = match stop {
        Some(b':') => place.key,
        Some(b',' | b']' | b'}') => place.flow,
        _ => false,
    };
    let mapping_meant = text.contains(": ") || text.ends_with(':');
    let whole = if place.flow || place.key_only {
        ends_node && !mapping_meant
    } else {
        !mapping_meant && !continues(src, at, place)
    };
    let valid = !text.is_empty() && fix.edits.first().is_some_and(|e| valid_single(&e.text));
    let fix = valid.then(|| Suggestion::new(fix.message, fix.edits, how(whole)));
    let label = if c == b'@' {
        "`@` is reserved"
    } else {
        "`` ` `` is reserved"
    };
    Polish {
        labels: vec![Label::new(Span::new(at, at.saturating_add(1)), label)],
        help: Some(
            "`@` and `` ` `` are reserved indicators (YAML 1.2 §5.3): a plain scalar \
             cannot start with them, so quote the value",
        ),
        fixes: fix.into_iter().collect(),
    }
}

/// A quoted scalar that is not closed where it should be: a label on its
/// opening quote and, maybe, the closing quote at the end of its first
/// line (never right after a backslash, which would escape it).
fn unclosed_quote(src: &str, code: Code, at: usize, q: u8) -> Polish {
    let (end, _) = value_end(src, at.saturating_add(1), false, false);
    let backslashes = src
        .get(at.saturating_add(1)..end)
        .map_or(0, |t| t.bytes().rev().take_while(|b| *b == b'\\').count());
    let escapes_quote = q == b'"' && backslashes % 2 == 1;
    let quote = if q == b'"' { "\"" } else { "'" };
    let fixes = if escapes_quote {
        Vec::new()
    } else {
        vec![Suggestion::new(
            "close the quote on this line",
            vec![Replacement::new(Span::empty(end), quote)],
            Applicability::MaybeIncorrect,
        )]
    };
    Polish {
        labels: vec![Label::new(
            Span::new(at, at.saturating_add(1)),
            "the quoted scalar starts here",
        )],
        help: (code == QUOTED_SCALAR_INDENTATION).then_some(
            "a quoted scalar goes on until its closing quote, and its continuation lines \
             must be indented more than its parent; if it should end on its first line, \
             add the closing quote there",
        ),
        fixes,
    }
}

/// What is glued to the closing quote of a quoted scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Glued {
    /// A `#` with no white space before it.
    Comment,
    /// Text that runs to the same quote again on this line (`'it's'`): an
    /// unescaped quote meant to be inside the scalar.
    InnerQuote(u8),
    /// Any other text (`'a'b c`, `"a"[x]`).
    Text(u8),
}

/// What is glued at `pos` to the closing quote of the quoted scalar
/// `qs..qe` (block context); `None` when nothing is. A pure function of
/// the source, scanning at most the rest of the line.
pub(super) fn glued(src: &str, (qs, qe): (usize, usize), pos: usize) -> Option<Glued> {
    let q = byte(src, qs).filter(|q| matches!(q, b'\'' | b'"'))?;
    if qe != pos || qe <= qs {
        return None;
    }
    if byte(src, pos) == Some(b'#') {
        return Some(Glued::Comment);
    }
    let single_line = src.get(qs..qe).is_some_and(|t| !t.contains(['\n', '\r']));
    let (rest_end, _) = value_end(src, pos, false, false);
    let closes = byte(src, rest_end.saturating_sub(1)) == Some(q);
    Some(if single_line && rest_end > pos && closes {
        Glued::InnerQuote(q)
    } else {
        Glued::Text(q)
    })
}

/// Content at `pos` glued to the closing quote of the quoted scalar
/// `qs..qe` (block context): an unescaped quote inside it (`'it's'`), a
/// comment without white space before `#`, or other text. `None` when not
/// glued.
pub(super) fn after_quote(
    src: &str,
    (qs, qe): (usize, usize),
    pos: usize,
    parent: Option<usize>,
) -> Option<Polish> {
    let kind = glued(src, (qs, qe), pos)?;
    let place = Place {
        key: false,
        key_only: false,
        parent,
        flow: false,
        sound: true,
    };
    let closing = Label::new(
        Span::new(qe.saturating_sub(1), qe),
        "the quoted scalar ends here",
    );
    let q = match kind {
        Glued::InnerQuote(q) => q,
        Glued::Text(_) => {
            return Some(Polish {
                labels: vec![closing],
                help: Some(
                    "only white space or a comment may follow a quoted scalar on its line; \
                     to keep the text, move it inside the quotes",
                ),
                fixes: Vec::new(),
            });
        }
        Glued::Comment => {
            let settled = !continues(src, pos, place);
            return Some(Polish {
                labels: vec![closing],
                help: Some("a comment needs white space before its `#` (§6.6)"),
                fixes: vec![Suggestion::new(
                    "separate the comment from the value",
                    vec![Replacement::new(Span::empty(pos), " ")],
                    how(settled),
                )],
            });
        }
    };
    let double = q == b'"';
    let (rest_end, _) = value_end(src, pos, false, false);
    let inner_end = rest_end.saturating_sub(1);
    // `glued` found the same quote again at `inner_end` on this line.
    // As `quote_fix`: a byte order mark in the glued text after
    // the closing quote is misplaced (`Y0032`, with its own fix), never
    // content, so the fix leaves it out rather than quoting an invisible
    // mark into the value. One inside the original quotes is the value's
    // (`nb-json`) and is kept.
    let fix = src
        .get(qs.saturating_add(1)..pos)
        .zip(src.get(pos..inner_end))
        .map(|(quoted, glued)| {
            let mut inner = String::with_capacity(quoted.len().saturating_add(glued.len()));
            inner.push_str(quoted);
            inner.extend(glued.chars().filter(|c| *c != '\u{feff}'));
            inner
        })
        .map(|inner| {
            let mut text = String::with_capacity(inner.len().saturating_add(4));
            text.push(char::from(q));
            text.push_str(&escape_quotes(&inner, double));
            text.push(char::from(q));
            (text, inner)
        })
        .filter(|(text, _)| {
            if double {
                valid_double(text)
            } else {
                valid_single(text)
            }
        })
        .map(|(text, _)| {
            let what = if double {
                "escape the inner `\"`"
            } else {
                "double the inner `'`"
            };
            let safe = !continues(src, pos, place);
            Suggestion::new(
                what,
                vec![Replacement::new(Span::new(qs, rest_end), text)],
                how(safe),
            )
        });
    let help = if double {
        "inside double quotes, write `\\\"` for a literal `\"`"
    } else {
        "inside single quotes, write `''` for a literal `'`"
    };
    Some(Polish {
        labels: vec![closing],
        help: Some(help),
        fixes: fix.into_iter().collect(),
    })
}
