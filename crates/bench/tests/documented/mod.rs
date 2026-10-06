//! The `EditError::Unsupported` reasons documented on `Document`, shared
//! by the edit fuzzes (`edit_fuzz.rs`, `stream_edit_fuzz.rs`): every
//! `Unsupported` refusal they meet must be one of these
//! (one list, so the two fuzzes cannot drift apart).
#![allow(dead_code)]

/// The `EditError::Unsupported` reasons documented on `Document`
/// ("Limitations in 0.2", the refusals listed after them, and the 0.3
/// limitation classes S1-S6 and P1-P7 of "Comments through node edits").
pub const DOCUMENTED_UNSUPPORTED: &[&str] = &[
    // Replacing a mapping key in place.
    "replacing a mapping key",
    // Entries whose key is a collection.
    "removing an entry with a collection key",
    // Block/flow moves of `Removed` that need re-rendering (the first is
    // also limitation class P1, the second S3/P3).
    "block content or comments cannot move into a flow collection",
    "a flow entry with comments around it cannot be re-inserted yet",
    "the removed text cannot be re-indented there",
    // Refusals of the request itself.
    "an alias inside its own anchored node",
    "an alias or error region cannot have an anchor",
    "a removed entry goes into a mapping, a removed item into a sequence",
    "removing a root node (use set_root)",
    "removing a key (remove its entry by value)",
    "set_scalar on a node that is not a scalar",
    // On the 0.2 list; the value of a `? key` entry written without `:` must
    // work (removed).
    "a tag written into a document with %TAG directives",
    // The 0.3 limitations, documented on `Document` (classes S1-S6). S3 flow
    // layout (also the flow re-insertion reason above):
    "removing a flow entry next to comments of other owners that the flow layout cannot keep",
    // S5:
    "comments of other owners between a collection's properties and its only entry",
    // S4:
    "removing the anchor of a node whose properties stand on a line of their own above \
     comment lines",
    // S6:
    "appending after comments that lie past an empty value (a `? ` key written without `:`)",
    // `remove_keep_comments` has no owner for the leading comments (a refusal
    // of the request, like removing a root; the call is the explicit request
    // that gives a root collection's last entry the document footer):
    "the leading comments of a flow entry or of a collection's only entry have no entry or \
     block footer to go to",
    // A nested collection's footers
    // when its only entry is removed (limitation P7); the entry's comment
    // inside a replaced value's first line where the new value cannot
    // carry it (`HEADER_COMMENT_LIMIT`, limitation P4).
    "the footer comments of a nested collection whose only entry is removed have no owner",
    "the entry's comment inside the replaced value cannot be written back there",
];
