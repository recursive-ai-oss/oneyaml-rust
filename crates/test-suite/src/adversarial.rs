//! The adversarial corpus for resource limits (each must be refused under
//! `untrusted()` with its code): inputs that `Limits::untrusted()` must reject
//! with a specific `Y2xxx` code and that `Limits::default()` must accept.
//!
//! Generated rather than vendored: the size cases are megabytes of
//! repetitive text. Each parser-level case is just over one `untrusted()`
//! limit and well under every `default()` limit.
//!
//! The alias-expansion cases (billion laughs, alias-to-merge-key
//! amplification, nested alias chains) are [`Layer::Meaning`]: the alias
//! budget applies to `Value` and serde only, never to the parser or the
//! syntax tree, so the parser accepts them and reading their
//! meaning under `untrusted()` fails with `Y2004`. Under `default()` each
//! does what [`Adversarial::default`] documents. The byte-expansion cases
//! (copies of a large anchored scalar, a long `%TAG` prefix
//! repeated per use) fail with `Y2009`, the alias byte budget.

use oneyaml_core::Code;
use oneyaml_core::code::{
    ALIAS_BUDGET_EXCEEDED, ALIAS_BYTE_BUDGET_EXCEEDED, DEPTH_LIMIT_EXCEEDED, DOCUMENT_TOO_LARGE,
    SCALAR_TOO_LONG, TOO_MANY_DOCUMENTS,
};

/// Where a limit applies.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// The parser rejects the input (an error event): every consumer.
    Parse,
    /// Only reading the meaning (`Value`, serde) rejects it: the parser
    /// and the syntax tree accept it (the alias budget).
    Meaning,
}

/// One adversarial input.
#[derive(Clone, Debug)]
pub struct Adversarial {
    /// Short description.
    pub name: &'static str,
    /// The input text.
    pub input: String,
    /// The limit code `untrusted()` must report first.
    pub code: Code,
    /// Where `code` is reported.
    pub layer: Layer,
    /// What `default()` does: `None`, accepts it; `Some(code)`, rejects it
    /// with that code (at the same layer).
    pub default: Option<Code>,
    /// Read with merge keys on (`Settings::merge_keys(true)`).
    pub merge_keys: bool,
}

/// `Limits::untrusted()` values the corpus is built around.
const DEPTH: usize = 128;
/// Half of [`DEPTH`]: each `[a: ` opens two levels (a sequence and a pair).
const HALF: usize = 64;
const SCALAR: usize = 1024 * 1024;
const SIZE: usize = 16 * 1024 * 1024;
const DOCUMENTS: usize = 1000;

fn case(name: &'static str, input: String, code: Code) -> Adversarial {
    Adversarial {
        name,
        input,
        code,
        layer: Layer::Parse,
        default: None,
        merge_keys: false,
    }
}

/// An alias-budget case: `untrusted()` allows max(2 × own nodes,
/// 100,000) expanded nodes, `default()` max(10 × own nodes, 1,000,000).
fn alias_case(name: &'static str, input: String, default: Option<Code>) -> Adversarial {
    Adversarial {
        name,
        input,
        code: ALIAS_BUDGET_EXCEEDED,
        layer: Layer::Meaning,
        default,
        merge_keys: false,
    }
}

/// An alias byte budget case: `untrusted()` allows max(2 × input
/// bytes, 8 MiB) bytes materialised by expansion, `default()` max(10 ×
/// input bytes, 64 MiB).
fn byte_case(name: &'static str, input: String, default: Option<Code>) -> Adversarial {
    Adversarial {
        code: ALIAS_BYTE_BUDGET_EXCEEDED,
        ..alias_case(name, input, default)
    }
}

/// A `len`-byte anchored scalar and `uses` aliases of it.
fn scalar_copies(len: usize, uses: usize) -> String {
    format!(
        "a: &a {}\nb: [{}]\n",
        "x".repeat(len),
        vec!["*a"; uses].join(", ")
    )
}

/// A `len`-byte `%TAG` prefix used by `uses` nodes.
fn tag_prefix_uses(len: usize, uses: usize) -> String {
    let mut out = format!("%TAG !e! tag:x,1:{}\n---\n", "a".repeat(len));
    for _ in 0..uses {
        out.push_str("- !e!x v\n");
    }
    out
}

/// `levels` anchored sequences, each holding `width` aliases of the one
/// before: the last expands to `width^levels` scalars.
fn alias_chain(levels: usize, width: usize) -> String {
    let mut out = String::from("l0: &l0 [lol]\n");
    for i in 1..=levels {
        let prev = i.saturating_sub(1);
        let refs = vec![format!("*l{prev}"); width].join(", ");
        out.push_str(&format!("l{i}: &l{i} [{refs}]\n"));
    }
    out
}

/// Alias-to-merge-key amplification: each level merges `width` aliases
/// of the one before, so the mapping stays at ten keys while the replayed
/// events grow `width`-fold per level (merged aliases count toward the
/// budget).
fn merge_chain(levels: usize, width: usize) -> String {
    let keys: Vec<String> = (0..10).map(|k| format!("k{k}: {k}")).collect();
    let mut out = format!("l0: &l0 {{{}}}\n", keys.join(", "));
    for i in 1..=levels {
        let prev = i.saturating_sub(1);
        let refs = vec![format!("*l{prev}"); width].join(", ");
        out.push_str(&format!("l{i}: &l{i} {{<<: [{refs}]}}\n"));
    }
    out
}

/// The corpus.
#[must_use]
pub fn corpus() -> Vec<Adversarial> {
    let over = DEPTH.saturating_add(1);
    let pairs = HALF.saturating_add(1);
    let flow_rest = over.saturating_sub(HALF);
    let big = SCALAR.saturating_add(1);
    let mut indented = String::new();
    for d in 0..over {
        indented.push_str(&" ".repeat(d));
        indented.push_str("k:\n");
    }
    // A plausible document just over 16 MiB: a mapping with one long block
    // scalar (still under the 64 MiB default scalar limit).
    let line = format!("  {}\n", "x".repeat(61));
    let lines = SIZE.checked_div(line.len()).unwrap_or(0).saturating_add(1);
    let oversized = format!("data: |\n{}", line.repeat(lines));
    vec![
        case(
            "deep block sequences",
            format!("{}a\n", "- ".repeat(over)),
            DEPTH_LIMIT_EXCEEDED,
        ),
        case("deep indented mappings", indented, DEPTH_LIMIT_EXCEEDED),
        case(
            "deep flow sequences",
            format!("{}{}\n", "[".repeat(over), "]".repeat(over)),
            DEPTH_LIMIT_EXCEEDED,
        ),
        case(
            "deep flow mappings",
            format!("{}x{}\n", "{a: ".repeat(over), "}".repeat(over)),
            DEPTH_LIMIT_EXCEEDED,
        ),
        case(
            "deep flow pairs",
            format!("{}x{}\n", "[a: ".repeat(pairs), "]".repeat(pairs)),
            DEPTH_LIMIT_EXCEEDED,
        ),
        case(
            "block nesting continued in flow",
            format!(
                "{}{}{}\n",
                "- ".repeat(HALF),
                "[".repeat(flow_rest),
                "]".repeat(flow_rest)
            ),
            DEPTH_LIMIT_EXCEEDED,
        ),
        case(
            "giant plain scalar",
            format!("k: {}\n", "a".repeat(big)),
            SCALAR_TOO_LONG,
        ),
        case(
            "giant double-quoted scalar",
            format!("k: \"{}\"\n", "a".repeat(big)),
            SCALAR_TOO_LONG,
        ),
        case(
            "giant literal block scalar",
            format!("k: |-\n  {}\n", "a".repeat(big)),
            SCALAR_TOO_LONG,
        ),
        case(
            "giant scalar in a flow sequence",
            format!("[{}]\n", "a".repeat(big)),
            SCALAR_TOO_LONG,
        ),
        case(
            "giant folded multi-line scalar",
            format!("k: >-\n{}", "  aaaaaaa\n".repeat(big.div_ceil(8))),
            SCALAR_TOO_LONG,
        ),
        case("oversized document", oversized, DOCUMENT_TOO_LARGE),
        case(
            "too many explicit documents",
            "--- a\n".repeat(DOCUMENTS.saturating_add(1)),
            TOO_MANY_DOCUMENTS,
        ),
        case(
            "too many documents ended with ...",
            "a\n...\n".repeat(DOCUMENTS.saturating_add(1)),
            TOO_MANY_DOCUMENTS,
        ),
        // 10^9 scalars: over every budget but `unlimited()`.
        alias_case(
            "billion laughs",
            alias_chain(9, 10),
            Some(ALIAS_BUDGET_EXCEEDED),
        ),
        // ~2^18 expanded nodes (every level is read): over `untrusted()`'s 100,000, under
        // `default()`'s 1,000,000.
        alias_case("nested alias chain", alias_chain(16, 2), None),
        // ~233,000 replayed nodes merging into ten keys: over
        // `untrusted()`, under `default()`.
        Adversarial {
            merge_keys: true,
            ..alias_case("alias-to-merge-key amplification", merge_chain(4, 10), None)
        },
        // 10 MB of copies of a 100 KB scalar: over `untrusted()`'s 8 MiB,
        // under `default()`'s 64 MiB.
        byte_case(
            "copies of a large scalar",
            scalar_copies(100_000, 100),
            None,
        ),
        // 1 GB of tag text from a 1 MB input: over every budget.
        byte_case(
            "long %TAG prefix repeated",
            tag_prefix_uses(1_000_000, 1000),
            Some(ALIAS_BYTE_BUDGET_EXCEEDED),
        ),
    ]
}
