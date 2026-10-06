//! Shared helper of the freeze-gate examples: a readable account of
//! who owns each comment, as `Document::explain_comments` reports it.

use oneyaml::{CommentOwner, Document, EditError, NodeId};

/// One line per comment, in source order, e.g.
/// ``# text -> leading of `name: http` (rule 4)``.
pub fn owners(doc: &Document) -> Result<Vec<String>, EditError> {
    let mut out = Vec::new();
    for e in doc.explain_comments()? {
        let owner = match e.owner() {
            CommentOwner::DocumentHeader => "document header".to_owned(),
            CommentOwner::DocumentFooter => "document footer".to_owned(),
            CommentOwner::Leading(id) => format!("leading of {}", name(doc, id)?),
            CommentOwner::Trailing(id) => format!("trailing of {}", name(doc, id)?),
            CommentOwner::Footer(id) => format!("footer of {}", name(doc, id)?),
            // `CommentOwner` is `#[non_exhaustive]`.
            _ => "another owner".to_owned(),
        };
        out.push(format!(
            "{} -> {owner} (rule {})",
            e.text(),
            e.rule().number()
        ));
    }
    Ok(out)
}

/// A node named for people: the value of a mapping entry by its key
/// (`` `jobs:` value ``), anything else by the first line of its text (an
/// entry is named by its key node, so `` `lint` ``).
fn name(doc: &Document, id: NodeId) -> Result<String, EditError> {
    let node = doc.node(id)?;
    Ok(match node.key() {
        Some(key) => format!("`{}:` value", key.text()),
        None => format!("`{}`", node.text().lines().next().unwrap_or_default()),
    })
}
