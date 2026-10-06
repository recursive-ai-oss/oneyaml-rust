//! Resource limits.

/// Resource limits: secondary safeguards against resource exhaustion.
///
/// Because parsing is iterative and O(n), no limit is needed for
/// *safety*; these bound memory and time on hostile input. Every limit that
/// is exceeded yields a limit diagnostic (`Y2xxx`) whose help names the
/// setting to raise, e.g. `raise Limits::max_depth (currently 128)`.
///
/// Three profiles:
///
/// | Limit | [`Limits::default()`] | [`Limits::untrusted()`] | [`Limits::unlimited()`] |
/// |---|---|---|---|
/// | `max_depth` | 1,024 | 128 | none |
/// | `alias_budget` | max(10 × own nodes, 1,000,000) | max(2 × own nodes, 100,000) | none |
/// | `alias_byte_budget` | max(10 × input bytes, 64 MiB) | max(2 × input bytes, 8 MiB) | none |
/// | `max_document_size` | 256 MiB | 16 MiB | none |
/// | `max_scalar_len` | 64 MiB | 1 MiB | none |
/// | `max_documents` | none | 1,000 | none |
/// | `max_typed_depth` | 128 | 64 | none |
/// | `max_diagnostics` | 100,000 | 1,000 | none |
///
/// The alias budgets bound what `Value` and serde reads materialise by
/// expanding aliases (nodes, and bytes beyond the input; see
/// [`Limits::alias_byte_budget`]). They are counted per document and
/// incrementally from the document's start, so a read of one node never
/// refuses what the whole document's read accepts. Each document of a
/// stream has its own budgets: reading a whole stream expands at most the
/// sum of its documents' budgets, so at most `max_documents` × the largest
/// one (with `untrusted()` and small documents: 1,000 × 100,000 nodes and
/// 1,000 × 8 MiB).
/// An alias whose whole expansion would exceed a budget is refused before
/// any of it is replayed, so a refused read costs about the input it read.
///
/// A numeric limit of `usize::MAX` means "no limit". Use
/// [`Limits::untrusted()`] for servers, webhooks, and APIs that parse input
/// from untrusted sources.
///
/// **`Limits::unlimited()`** removes every bound, and it is the only way to
/// disable the alias budgets (nodes and bytes; `alias_budget(usize::MAX)`
/// and `alias_byte_budget(usize::MAX)` make them unreachable instead). Use
/// it only for input you trust completely.
///
/// Compatibility presets never change limits: safety is not a compat
/// behaviour.
///
/// # Examples
///
/// ```
/// use oneyaml_core::Limits;
///
/// // A service reading small uploads: the untrusted profile, tightened.
/// let l = Limits::untrusted()
///     .max_depth(32)
///     .max_typed_depth(32)
///     .max_document_size(64 * 1024)
///     .max_scalar_len(4 * 1024)
///     .max_documents(1)
///     .max_diagnostics(10)
///     .alias_budget(1_000)
///     .alias_byte_budget(256 * 1024);
/// assert_eq!(l.get_max_depth(), 32);
/// assert_eq!(l.get_max_typed_depth(), 32);
/// assert_eq!(l.get_max_document_size(), 64 * 1024);
/// assert_eq!(l.get_max_scalar_len(), 4 * 1024);
/// assert_eq!(l.get_max_documents(), 1);
/// assert_eq!(l.get_max_diagnostics(), 10);
/// // A fixed budget ignores the document's size ...
/// assert_eq!(l.get_alias_budget(1_000_000), Some(1_000));
/// assert_eq!(l.get_alias_byte_budget(1 << 30), Some(256 * 1024));
/// // ... a ratio scales with it, above a floor.
/// let r = l.alias_budget_ratio(3, 100).alias_byte_budget_ratio(2, 4096);
/// assert_eq!(r.get_alias_budget(10), Some(100));
/// assert_eq!(r.get_alias_budget(1_000), Some(3_000));
/// assert_eq!(r.get_alias_byte_budget(10_000), Some(20_000));
/// // `unlimited()` is the only profile without alias budgets.
/// assert_eq!(Limits::unlimited().get_alias_budget(1), None);
/// assert_eq!(Limits::unlimited().get_alias_byte_budget(1), None);
/// ```
// Value type, private fields, chainable `const fn` setters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    max_depth: usize,
    alias_factor: usize,
    alias_floor: usize,
    alias_enabled: bool,
    alias_bytes_factor: usize,
    alias_bytes_floor: usize,
    alias_bytes_enabled: bool,
    max_document_size: usize,
    max_scalar_len: usize,
    max_documents: usize,
    max_typed_depth: usize,
    max_diagnostics: usize,
}

const MIB: usize = 1024 * 1024;

impl Limits {
    /// The default profile. Same as [`Limits::default()`], usable in `const`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_depth: 1024,
            alias_factor: 10,
            alias_floor: 1_000_000,
            alias_enabled: true,
            alias_bytes_factor: 10,
            alias_bytes_floor: 64 * MIB,
            alias_bytes_enabled: true,
            max_document_size: 256 * MIB,
            max_scalar_len: 64 * MIB,
            max_documents: usize::MAX,
            max_typed_depth: 128,
            max_diagnostics: 100_000,
        }
    }

    /// The profile for untrusted input.
    #[must_use]
    pub const fn untrusted() -> Self {
        Self {
            max_depth: 128,
            alias_factor: 2,
            alias_floor: 100_000,
            alias_enabled: true,
            alias_bytes_factor: 2,
            alias_bytes_floor: 8 * MIB,
            alias_bytes_enabled: true,
            max_document_size: 16 * MIB,
            max_scalar_len: MIB,
            max_documents: 1000,
            max_typed_depth: 64,
            max_diagnostics: 1000,
        }
    }

    /// No limits at all, including no alias budget. Only for trusted input.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_depth: usize::MAX,
            alias_factor: 0,
            alias_floor: usize::MAX,
            alias_enabled: false,
            alias_bytes_factor: 0,
            alias_bytes_floor: usize::MAX,
            alias_bytes_enabled: false,
            max_document_size: usize::MAX,
            max_scalar_len: usize::MAX,
            max_documents: usize::MAX,
            max_typed_depth: usize::MAX,
            max_diagnostics: usize::MAX,
        }
    }

    /// Maximum nesting depth of collections, counting block and flow
    /// collections together; a top-level collection has depth 1.
    ///
    /// Consumed by the parser now; the tree builder, `Value`/serde, and the
    /// emitter will enforce it too.
    #[must_use]
    pub const fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// A fixed alias budget: at most `nodes` nodes may be produced by alias
    /// expansion (including merge keys through aliases).
    ///
    /// Applies to `Value` and serde only, never to `Document`.
    #[must_use]
    pub const fn alias_budget(mut self, nodes: usize) -> Self {
        self.alias_factor = 0;
        self.alias_floor = nodes;
        self.alias_enabled = true;
        self
    }

    /// A ratio alias budget: `max(factor × own nodes, floor)`, where "own
    /// nodes" is the number of nodes the document itself contains.
    ///
    /// Applies to `Value` and serde only, never to `Document`.
    #[must_use]
    pub const fn alias_budget_ratio(mut self, factor: usize, floor: usize) -> Self {
        self.alias_factor = factor;
        self.alias_floor = floor;
        self.alias_enabled = true;
        self
    }

    /// A fixed alias **byte** budget: at most `bytes` bytes may
    /// be materialised by alias expansion beyond the input.
    ///
    /// Counted per document, incrementally, by `Value` and serde reads only
    /// (never by `Document`, which expands nothing):
    ///
    /// * every scalar value, key, and tag that alias expansion copies
    ///   (merge keys through aliases included), in full;
    /// * each tag use's `%TAG` prefix, which a tag shares rather than
    ///   copies but which its logical text (and so any output of it)
    ///   repeats;
    /// * the text of a `%TAG` tag joined into an enum variant name that
    ///   names no variant of the target (`#[serde(other)]`, hand-written
    ///   visitors).
    ///
    /// Exceeding it is `Y2009` (`code::ALIAS_BYTE_BUDGET_EXCEEDED`).
    /// Independent of [`Limits::alias_budget`], which counts nodes.
    #[must_use]
    pub const fn alias_byte_budget(mut self, bytes: usize) -> Self {
        self.alias_bytes_factor = 0;
        self.alias_bytes_floor = bytes;
        self.alias_bytes_enabled = true;
        self
    }

    /// A ratio alias byte budget: `max(factor × input bytes, floor)`, where
    /// "input bytes" is how much of the document (from its start, `---` or
    /// its first content) has been read at the point of the charge, so a
    /// read of one node has the budget the whole read has at the same
    /// place. See [`Limits::alias_byte_budget`] for what is charged.
    #[must_use]
    pub const fn alias_byte_budget_ratio(mut self, factor: usize, floor: usize) -> Self {
        self.alias_bytes_factor = factor;
        self.alias_bytes_floor = floor;
        self.alias_bytes_enabled = true;
        self
    }

    /// Maximum input size in bytes (the whole stream, after decoding to
    /// UTF-8).
    ///
    /// Consumed by the parser now; readers will also check it before
    /// reading.
    #[must_use]
    pub const fn max_document_size(mut self, bytes: usize) -> Self {
        self.max_document_size = bytes;
        self
    }

    /// Maximum length in bytes of one scalar's value (after escapes and
    /// folding). Consumed by the parser.
    #[must_use]
    pub const fn max_scalar_len(mut self, bytes: usize) -> Self {
        self.max_scalar_len = bytes;
        self
    }

    /// Maximum number of documents in one stream. Consumed by the parser.
    #[must_use]
    pub const fn max_documents(mut self, documents: usize) -> Self {
        self.max_documents = documents;
        self
    }

    /// Maximum nesting of collections that serde visits through the target
    /// type's own `Deserialize` impls, i.e. how deep a *recursive user
    /// type* such as `struct Tree(Vec<Tree>)` may follow the input.
    ///
    /// serde's visitor model recurses once per such level, so this bounds
    /// the native stack: measured at about 1.9 KiB per level in release
    /// builds and 7.4 KiB in debug builds (`examples/stack_probe.rs` in
    /// `oneyaml`; the cost depends on the target type). The defaults (128;
    /// 64 untrusted) fit a 2 MiB thread stack in both. **Before raising it,
    /// make sure the deserializing thread's stack holds `max_typed_depth ×`
    /// the per-level cost**, e.g. run on a thread built with
    /// `std::thread::Builder::stack_size`; [`Limits::unlimited()`] removes
    /// the bound entirely, so only use it with non-recursive targets or
    /// input you trust. `Value` targets (and `Value` fields) are built
    /// iteratively and are bounded only by `max_depth`.
    #[must_use]
    pub const fn max_typed_depth(mut self, depth: usize) -> Self {
        self.max_typed_depth = depth;
        self
    }

    /// Maximum number of diagnostics (errors and warnings together) one
    /// parse or read keeps.
    ///
    /// The budget is per **stream**, shared by errors and warnings, and
    /// counts every kind (decoding, settings, syntax, meaning). A list of
    /// diagnostics keeps its first `max_diagnostics` entries in the order
    /// it reports them; when more were found, it also keeps the first
    /// error (whatever its place: it is the one a strict parse fails with)
    /// and then ends with **one summary** warning, `Y2010`
    /// (`code::DIAGNOSTICS_SUPPRESSED`): "N more diagnostics suppressed",
    /// located where the first suppressed one was, whose help names this
    /// setting. The summary does not count toward the limit (a capped list
    /// holds at most `max_diagnostics + 2` entries) and N is exact.
    /// Exceeding the limit is never an error by itself: strict parses still
    /// fail exactly at the first error, and only then.
    ///
    /// **Meaning never depends on it**: what a read accepts or
    /// refuses, `has_errors`, restricted mode, which edits are accepted, and
    /// a read's cost are the same under every limit. Past it, the syntax
    /// tree keeps a diagnostic as a 32-byte skeleton (code, severity,
    /// spans) without its texts, and an error reported from there has its
    /// code's summary for a message. Paths that discard warnings build at most 16.
    #[must_use]
    pub const fn max_diagnostics(mut self, diagnostics: usize) -> Self {
        self.max_diagnostics = diagnostics;
        self
    }

    /// The `max_diagnostics` limit (`usize::MAX`: none).
    #[must_use]
    pub const fn get_max_diagnostics(&self) -> usize {
        self.max_diagnostics
    }

    /// The `max_typed_depth` limit (`usize::MAX`: none).
    #[must_use]
    pub const fn get_max_typed_depth(&self) -> usize {
        self.max_typed_depth
    }

    /// The `max_depth` limit (`usize::MAX`: none).
    #[must_use]
    pub const fn get_max_depth(&self) -> usize {
        self.max_depth
    }

    /// The alias budget for a document with `own_nodes` nodes, or `None`
    /// when the budget is disabled ([`Limits::unlimited()`]).
    #[must_use]
    pub const fn get_alias_budget(&self, own_nodes: usize) -> Option<usize> {
        if !self.alias_enabled {
            return None;
        }
        let scaled = self.alias_factor.saturating_mul(own_nodes);
        Some(if scaled > self.alias_floor {
            scaled
        } else {
            self.alias_floor
        })
    }

    /// The alias byte budget after `input_bytes` bytes of the document, or
    /// `None` when it is disabled ([`Limits::unlimited()`]).
    #[must_use]
    pub const fn get_alias_byte_budget(&self, input_bytes: usize) -> Option<usize> {
        if !self.alias_bytes_enabled {
            return None;
        }
        let scaled = self.alias_bytes_factor.saturating_mul(input_bytes);
        Some(if scaled > self.alias_bytes_floor {
            scaled
        } else {
            self.alias_bytes_floor
        })
    }

    /// The `max_document_size` limit in bytes (`usize::MAX`: none).
    #[must_use]
    pub const fn get_max_document_size(&self) -> usize {
        self.max_document_size
    }

    /// The `max_scalar_len` limit in bytes (`usize::MAX`: none).
    #[must_use]
    pub const fn get_max_scalar_len(&self) -> usize {
        self.max_scalar_len
    }

    /// The `max_documents` limit (`usize::MAX`: none).
    #[must_use]
    pub const fn get_max_documents(&self) -> usize {
        self.max_documents
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_have_the_q4_5_values() {
        let d = Limits::default();
        assert_eq!(d, Limits::new());
        assert_eq!(d.get_max_depth(), 1024);
        assert_eq!(d.get_alias_budget(10), Some(1_000_000));
        assert_eq!(d.get_alias_budget(200_000), Some(2_000_000));
        assert_eq!(d.get_max_document_size(), 256 * 1024 * 1024);
        assert_eq!(d.get_max_scalar_len(), 64 * 1024 * 1024);
        assert_eq!(d.get_max_documents(), usize::MAX);
        assert_eq!(d.get_max_typed_depth(), 128);
        assert_eq!(d.get_max_diagnostics(), 100_000);
        assert_eq!(d.get_alias_byte_budget(0), Some(64 * 1024 * 1024));
        assert_eq!(
            d.get_alias_byte_budget(10 * 1024 * 1024),
            Some(100 * 1024 * 1024)
        );

        let u = Limits::untrusted();
        assert_eq!(u.get_max_depth(), 128);
        assert_eq!(u.get_alias_budget(10), Some(100_000));
        assert_eq!(u.get_alias_budget(60_000), Some(120_000));
        assert_eq!(u.get_max_document_size(), 16 * 1024 * 1024);
        assert_eq!(u.get_max_scalar_len(), 1024 * 1024);
        assert_eq!(u.get_max_documents(), 1000);
        assert_eq!(u.get_max_typed_depth(), 64);
        assert_eq!(u.get_max_diagnostics(), 1000);
        assert_eq!(u.get_alias_byte_budget(1000), Some(8 * 1024 * 1024));
        assert_eq!(
            u.get_alias_byte_budget(5 * 1024 * 1024),
            Some(10 * 1024 * 1024)
        );

        let n = Limits::unlimited();
        assert_eq!(n.get_max_depth(), usize::MAX);
        assert_eq!(n.get_alias_budget(usize::MAX), None);
        assert_eq!(n.get_max_document_size(), usize::MAX);
        assert_eq!(n.get_max_scalar_len(), usize::MAX);
        assert_eq!(n.get_max_documents(), usize::MAX);
        assert_eq!(n.get_max_typed_depth(), usize::MAX);
        assert_eq!(n.get_max_diagnostics(), usize::MAX);
        assert_eq!(n.get_alias_byte_budget(usize::MAX), None);
    }

    #[test]
    fn setters_chain_and_are_const() {
        const L: Limits = Limits::untrusted()
            .max_depth(7)
            .max_document_size(8)
            .max_scalar_len(9)
            .max_documents(10)
            .alias_budget(11)
            .max_typed_depth(12)
            .max_diagnostics(13);
        assert_eq!(L.get_max_typed_depth(), 12);
        assert_eq!(L.get_max_diagnostics(), 13);
        assert_eq!(
            (
                L.get_max_depth(),
                L.get_max_document_size(),
                L.get_max_scalar_len(),
                L.get_max_documents(),
                L.get_alias_budget(1_000),
            ),
            (7, 8, 9, 10, Some(11))
        );
        let r = Limits::unlimited().alias_budget_ratio(3, 5);
        assert_eq!(r.get_alias_budget(1), Some(5));
        assert_eq!(r.get_alias_budget(4), Some(12));
        assert_eq!(r.get_alias_budget(usize::MAX), Some(usize::MAX));
        assert_ne!(r, Limits::unlimited());
        // The byte budget is set on its own; the node budget stays off.
        const B: Limits = Limits::unlimited().alias_byte_budget(7);
        assert_eq!(B.get_alias_byte_budget(1_000_000), Some(7));
        assert_eq!(B.get_alias_budget(1), None);
        let b = Limits::unlimited().alias_byte_budget_ratio(3, 5);
        assert_eq!(b.get_alias_byte_budget(1), Some(5));
        assert_eq!(b.get_alias_byte_budget(4), Some(12));
        assert_eq!(b.get_alias_byte_budget(usize::MAX), Some(usize::MAX));
        assert_eq!(b.get_alias_budget(1), None);
        // And the node budget leaves the byte budget alone.
        let n = Limits::default().alias_budget(3);
        assert_eq!(n.get_alias_byte_budget(0), Some(64 * 1024 * 1024));
    }
}
