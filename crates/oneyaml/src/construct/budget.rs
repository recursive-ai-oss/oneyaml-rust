//! The alias **byte** budget of one read.
//!
//! The node budget ([`super::expand`]) bounds how many nodes alias
//! expansion produces; this bounds how many bytes it materialises beyond
//! the input, so the logical size of what a read builds (a `Value`, the
//! strings handed to a typed visitor) — and so of any output written from
//! it (emitter, `Debug`, serde) — is O(input + budget):
//!
//! * a replayed event costs its scalar text and its tag's logical length
//!   (scalar keys are scalars; merges through aliases are replays);
//! * an own event costs its tag's shared `%TAG` prefix, the part of its
//!   logical text that is not at the place of use in the input (a tag
//!   shares it in memory, but its text, and every output of it, repeats
//!   it per use);
//! * a `%TAG` tag joined into an enum variant name that names no variant
//!   costs the joined text (`de/scalar.rs`).
//!
//! The budget is per document and incremental, like the node budget:
//! `Limits::get_alias_byte_budget(bytes of the document read so far)`,
//! measured from the document's start (its `DocStart` event, `---` or its
//! first content) to the end of the last source event read. A per-node read
//! measures from the same document start, so at any event it has the budget
//! the whole read has there; it is charged only for what the node itself
//! expands, so it never refuses what the whole read accepts (the
//! prefix rule).
//!
//! Every charge is O(1). The meter is shared (`Rc`, `Cell`: `alloc`
//! and `core` only) between the expander and the variant-name
//! matching of the same read, in the one allocation the read already made
//! for its variant memo ([`ReadShared`]: allocation counts unchanged).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use oneyaml_core::{Limits, Span, code};

use crate::de::scalar::VariantMemo;
use crate::error::Error;

/// What the parts of one read share: the byte meter (the expander charges
/// expansion, the deserializer joined variant names) and the variant memo
/// for typed enums.
pub(crate) struct ReadShared {
    pub(crate) meter: ByteMeter,
    pub(crate) variants: RefCell<VariantMemo>,
}

/// [`ReadShared`], shared.
pub(crate) type SharedRead = Rc<ReadShared>;

impl ReadShared {
    pub(crate) fn new(limits: &Limits) -> SharedRead {
        Rc::new(Self {
            meter: ByteMeter::new(limits),
            variants: RefCell::default(),
        })
    }
}

/// One read's byte meter.
#[derive(Debug)]
pub(crate) struct ByteMeter {
    limits: Limits,
    /// Bytes charged in the current document.
    spent: Cell<usize>,
    /// Bytes of the current document read so far (what the budget scales
    /// with).
    input: Cell<usize>,
}

impl ByteMeter {
    pub(crate) fn new(limits: &Limits) -> Self {
        Self {
            limits: limits.clone(),
            spent: Cell::new(0),
            input: Cell::new(0),
        }
    }

    /// A new document: nothing spent, nothing read.
    pub(crate) fn reset(&self) {
        self.spent.set(0);
        self.input.set(0);
    }

    /// `bytes` of the document have been read (monotone).
    pub(crate) fn read_to(&self, bytes: usize) {
        if bytes > self.input.get() {
            self.input.set(bytes);
        }
    }

    /// The budget now (`None`: disabled).
    fn budget(&self) -> Option<usize> {
        self.limits.get_alias_byte_budget(self.input.get())
    }

    /// Whether `bytes` more fit the budget now, without charging them.
    ///
    /// # Errors
    ///
    /// `Y2009` at `span` when they do not.
    pub(crate) fn check(&self, bytes: usize, span: Option<Span>) -> Result<(), Error> {
        match self.budget() {
            Some(budget) if self.spent.get().saturating_add(bytes) > budget => {
                Err(alias_byte_budget_exceeded(budget, self.input.get(), span))
            }
            _ => Ok(()),
        }
    }

    /// Charges `bytes`.
    ///
    /// # Errors
    ///
    /// `Y2009` at `span` when the budget is exceeded (nothing is charged).
    pub(crate) fn charge(&self, bytes: usize, span: Option<Span>) -> Result<(), Error> {
        if bytes == 0 {
            return Ok(());
        }
        self.check(bytes, span)?;
        self.spent.set(self.spent.get().saturating_add(bytes));
        Ok(())
    }
}

/// The `Y2009` limit error (the help names the setting).
pub(crate) fn alias_byte_budget_exceeded(budget: usize, input: usize, span: Option<Span>) -> Error {
    Error::new(
        code::ALIAS_BYTE_BUDGET_EXCEEDED,
        format!(
            "expansion (alias copies and `%TAG` prefixes repeated per use) \
             materialises more than {budget} bytes \
             (after {input} bytes of the document)"
        ),
        span,
    )
    .with_help(format!(
        "raise Limits::alias_byte_budget (currently {budget} for this document), \
         or use Limits::unlimited() for fully trusted input"
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn charges_up_to_the_budget_and_scales_with_the_input() {
        let m = ByteMeter::new(&Limits::unlimited().alias_byte_budget_ratio(2, 10));
        m.charge(10, None).unwrap();
        let e = m.charge(1, None).unwrap_err();
        assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
        assert!(
            e.diagnostic()
                .help
                .as_deref()
                .unwrap()
                .contains("alias_byte_budget")
        );
        m.read_to(6);
        m.read_to(3);
        m.check(2, None).unwrap();
        m.charge(2, None).unwrap();
        assert!(m.check(1, None).is_err());
        m.reset();
        m.charge(10, None).unwrap();
        let off = ByteMeter::new(&Limits::unlimited());
        off.charge(usize::MAX, None).unwrap();
        off.charge(usize::MAX, None).unwrap();
        off.charge(0, None).unwrap();
    }
}
