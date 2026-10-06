//! The packed per-node payload of scalar and collection start events (kept
//! small): property and block header positions as 32-bit offsets back from the
//! node's start, with a boxed absolute fallback for inputs past 4 GiB.

use oneyaml_core::Span;

use super::{BlockHeader, Chomping, Properties};

/// A node event's properties and, for a block scalar, its header.
///
/// Opaque and compact (24 bytes): read it through [`Event::properties`],
/// [`Event::block_header`], or [`NodeInfo::properties`] with the node's
/// start. Two `NodeInfo`s built for the same node start compare equal
/// exactly when they describe the same properties and header.
///
/// [`Event::properties`]: super::Event::properties
/// [`Event::block_header`]: super::Event::block_header
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeInfo(Repr);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Repr {
    Compact(Compact),
    Wide(Box<Wide>),
}

/// Offsets back from the node's start; `0` means "absent" (every property
/// and header lies strictly before its node's start, C3/C5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Compact {
    anchor: u32,
    tag: u32,
    prefix: u32,
    header: u32,
    indent: u32,
    header_len: u8,
    indicator: u8,
    chomping: Chomping,
}

const NONE: Compact = Compact {
    anchor: 0,
    tag: 0,
    prefix: 0,
    header: 0,
    indent: 0,
    header_len: 0,
    indicator: 0,
    chomping: Chomping::Clip,
};

/// Absolute positions, for what does not fit [`Compact`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Wide {
    props: Properties,
    block: Option<BlockHeader>,
}

/// `start - at` as a non-zero `u32`, or `None` (then the wide form is used).
fn back(start: usize, at: usize) -> Option<u32> {
    start
        .checked_sub(at)
        .filter(|d| *d > 0)
        .and_then(|d| u32::try_from(d).ok())
}

/// Like [`back`], with `None` for an absent position.
fn back_opt(start: usize, at: Option<usize>) -> Option<u32> {
    at.map_or(Some(0), |at| back(start, at))
}

fn compact(start: usize, props: Properties, block: Option<BlockHeader>) -> Option<Compact> {
    let mut c = Compact {
        anchor: back_opt(start, props.anchor)?,
        tag: back_opt(start, props.tag)?,
        prefix: back_opt(start, props.tag_prefix)?,
        ..NONE
    };
    if let Some(h) = block {
        c.header = back(start, h.span.start)?;
        c.header_len = u8::try_from(h.span.end.checked_sub(h.span.start)?).ok()?;
        c.indent = u32::try_from(h.content_indent).ok()?;
        c.indicator = match h.indent_indicator {
            Some(0) => return None,
            Some(i) => i,
            None => 0,
        };
        c.chomping = h.chomping;
    }
    Some(c)
}

/// `start - back`, or `None` for an absent (`0`) offset.
fn at(start: usize, back: u32) -> Option<usize> {
    if back == 0 {
        return None;
    }
    start.checked_sub(usize::try_from(back).ok()?)
}

impl NodeInfo {
    /// No properties and no header.
    #[must_use]
    pub const fn none() -> Self {
        Self(Repr::Compact(NONE))
    }

    /// The info of a node starting at `start`, with its properties and
    /// block header at absolute positions.
    #[must_use]
    pub fn new(start: usize, props: Properties, block: Option<BlockHeader>) -> Self {
        if block.is_none() && props == Properties::none() {
            return Self::none();
        }
        match compact(start, props, block) {
            Some(c) => Self(Repr::Compact(c)),
            None => Self(Repr::Wide(Box::new(Wide { props, block }))),
        }
    }

    /// The properties of the node starting at `start`.
    #[must_use]
    pub fn properties(&self, start: usize) -> Properties {
        match &self.0 {
            Repr::Compact(c) => {
                Properties::new(at(start, c.anchor), at(start, c.tag), at(start, c.prefix))
            }
            Repr::Wide(w) => w.props,
        }
    }

    /// The block header of the scalar starting at `start`.
    #[must_use]
    pub fn block_header(&self, start: usize) -> Option<BlockHeader> {
        match &self.0 {
            Repr::Compact(c) => {
                let from = at(start, c.header)?;
                let span = Span::new(from, from.saturating_add(usize::from(c.header_len)));
                let indicator = (c.indicator != 0).then_some(c.indicator);
                let indent = usize::try_from(c.indent).ok()?;
                Some(BlockHeader::new(c.chomping, indicator, indent, span))
            }
            Repr::Wide(w) => w.block,
        }
    }

    /// `true` when the node has neither an anchor nor a tag.
    #[must_use]
    #[doc(hidden)]
    pub fn has_no_properties(&self) -> bool {
        match &self.0 {
            Repr::Compact(c) => c.anchor == 0 && c.tag == 0,
            Repr::Wide(w) => w.props.is_empty(),
        }
    }

    /// Whether the compact form is used (tests only).
    #[cfg(test)]
    pub(crate) const fn is_compact(&self) -> bool {
        matches!(self.0, Repr::Compact(_))
    }
}

impl Default for NodeInfo {
    fn default() -> Self {
        Self::none()
    }
}
