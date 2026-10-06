# Comment attachment rulebook

> **Status: normative.** Implemented by `oneyaml-syntax`
> (`tree/attach.rs`: `SyntaxTree::comment_attachment`,
> `SyntaxTree::comment_attachments`) and explained by
> `oneyaml::Document::explain_comment`.
>
> **Stability.** Rule numbers are stable once published: a number is never
> reused or renumbered, new rules are appended. Owner assignments do not
> change within a major version except to fix a clear misattribution,
> called out in the changelog. Every rule has
> golden tests (`crates/oneyaml/tests/comment_golden.rs` and its
> snapshot).
>
> Rules 1–9 are the core rules; rules 10–13 refine them. Section 6 lists
> the cases where the rules needed a decision, with the choice made and
> why.

## 1. Vocabulary

- **Comment**: a `#` comment token of the syntax tree, from `#` to the end
  of its line (storage is byte-exact; nothing here changes bytes).
- **Content token**: a token with bytes that is not white space, a line
  break, a comment, or a byte order mark (scalar text, indicators `-` `?`
  `:` `,`, brackets, properties `&a` `!t`, block scalar headers, alias
  names, directives, `---`, `...`, error bytes).
- **Line**: lines end at line break tokens (LF, CRLF, or CR, §5.4) and at
  the end of a block scalar's text, which ends with its own line break.
- **Same-line comment**: a comment with a content token before it on its
  line. **Own-line comment**: any other comment.
- **P** (previous) and **N** (next): the content tokens right before and
  right after a comment, ignoring every comment, white space, line break,
  and zero-width token between them. A **run** is the stretch between one
  P and its N; all comments of a run share P, N, and everything derived
  from them.
- **Column** of a token: the number of characters (Unicode scalar values;
  a tab counts as one) between the start of its line and the token. A
  byte order mark counts as no column (the parser resets the column after
  it, YAML 1.2.2 §5.2).
- **Entry**: a mapping key/value pair, named by its **key** node; it starts
  at its `?` when it has one, else at its key. **Item**: a sequence
  element, named by its node; a block item starts at its `-`, a flow item
  at its node. An entry's or item's comments are its key's or node's.
- **Root**: the document's first node that has bytes. A document with no
  such node has **no visible root** (e.g. `---` alone).
- **Block collection's column**: the column where its first entry or item
  starts (its own `&anchor`/`!tag` properties are not part of it).
- **Closing collections** of a run: the block collections that contain P
  but not N (they end with P), innermost first. Their columns never
  increase from inner to outer in a well-formed tree.
- **What N opens** (the *target* of a leading comment): the innermost entry
  or item that starts at N; else the outermost node that starts at N; else,
  when N is the `:` of an explicit entry, that entry's value; else the
  innermost node that holds N.

## 2. Precedence

Every comment falls to exactly one rule, decided in this order:

1. A **same-line** comment: **rule 5**, always.
2. An **own-line** comment: find its document context (section 4), then:
   1. before the root (or in a document without visible root):
      **rule 1**, else **rule 2**, else **rule 3**;
   2. inside the root, in a flow collection: **rule 9**;
   3. inside the root, in block context: **rule 6** if it applies, else
      **rule 10** if the comment is left of N, else **rule 4 / 11 / 12**
      by what N opens;
   4. after the root: **rule 6** if it applies, else **rule 7** — or, for
      a comment between documents: **rule 1** whenever a `---` follows,
      else rule 6, else the next document's header rules (2, 3), else
      rule 7.

3. Then **rule 13** over each run, in source order: an own-line comment
   whose owner from step 2 is deeper than the owner of an earlier comment
   of its run takes the owner of the comment before it.

Rule 8 (blank lines) decides no owner: it is a count every comment carries.

## 3. The rules

### Rule 1 — Document header, `---` form
An own-line comment **before the document's `---`** is the **document
header**, whatever blank lines surround it and whatever its column.
This includes comments among the `%YAML`/`%TAG` directives, and every
comment between the previous document and this one's `---` — also an
indented one after the previous document's last block collection
(`items:⏎  - a⏎  # - b⏎---`: `# - b` is the next document's header; rule
6 never reaches past a `---`; see section 6, R1).

### Rule 2 — Document header, blank-line form
An own-line comment before the root (after `---`, or at the top of a
document written without `---`) with **a blank line somewhere between it
and the root** is the **document header**. An own-line comment in a
document **without a visible root** is also document header — including
the comments after such a document when it ends without `...` and no
next document follows (`--- ⏎# c`) — as is a comment of an input that has
no document at all (its owner is then the stream: `DocumentHeader` with no
document).

### Rule 3 — Document header, symmetric case: first node's leading
An own-line comment before the root with **no blank line between it and
the root** is a **leading comment of what the root's first token opens**:
the root mapping's first entry, the root sequence's first item, else the
root node itself (e.g. a root mapping with an `&anchor` line, or a scalar).

*Choice:* "the first node" is the innermost entry or item
the root opens with, not the root collection. A comment directly above
`a: 1` is about the entry `a`; a comment about the whole document is
separated by a blank line (rule 2) or placed before `---` (rule 1). The
column condition of rule 4 does not apply at the top (there is nothing
shallower for the comment to belong to).

### Rule 4 — Leading comments
An own-line comment inside the root, in block context, followed (only
comments, white space and line breaks between — **blank lines included**,
D1 in section 6) by content token N,
**at or right of N's column**, is a **leading comment of what N opens**:
the entry whose key or `?` N is, the item whose `-` N is, else the node N
starts (a value on the line below its key, `k:⏎  # c⏎  v`, leads `v`; a
collection starting with an `&anchor` line leads the collection) — unless
rule 6 applies first.

### Rule 5 — Trailing comments
A same-line comment belongs to the **innermost entry or item containing P**
(the content token before it on its line), found by walking up from P:

- in a mapping, the entry whose key, `?`, `:`, value, or (flow) `,` P is
  in — so for `key:  # c` with a block value on the next lines, the owner is
  the **entry**, and properties on the key line
  (`k: &a # c`) belong to it too;
- in a sequence, the item whose `-`, node, or (flow) `,` P is in;
- a collection's own bracket or properties are skipped, so `k: [ # c`
  trails the entry `k` and `a: [1, 2] # c` trails the entry `a`;
- P in the root but in no entry (a root scalar, a root flow collection's
  brackets): **trailing comment of the root**;
- P a directive or `---`: **document header**; P a `...`: **document
  footer**; P a stray `...` between documents, or content of an error
  region outside every document: the next document's header, else the
  previous document's footer, else (no document at all) the stream's
  header;
- P error bytes skipped at document level (error recovery): **document
  footer** (section 5).

### Rule 6 — Collection footer
An own-line comment after a block collection's last child is that
collection's **footer** when its column is **right of the column of what
follows at the parent level**:

- inside the root, "what follows" is N: the rule applies when the run has
  closing collections and the comment's column is greater than N's;
- after the root, "what follows" is the root's level: the rule applies
  when the run has closing collections (the root is block) and the column
  is greater than the root's column. After an implicitly ended document
  this holds at stream level only when **no `---` follows** (the last
  document, or a next document written without `---` in recovered input):
  a comment before a `---` is always rule 1;
- the owner is the **innermost closing collection whose column is at or
  left of the comment's column**; if every closing collection is right of
  it, the outermost one.

A comment after a document's explicit `...` never belongs to that
document's collections.

*Choice (rule 4 vs rules 6/7):*
comment lines after a block scalar are decided like any other comment
lines. After a **middle** entry they lead the next entry (rule 4, or 10
when shallower) — a block scalar is not a collection, so it has no footer.
After the **last** entry they are the enclosing collection's footer (rule
6) or the document footer (rule 7), by column.

### Rule 7 — Document footer
An own-line comment after the root's last token that rule 6 does not claim
(its column is at or left of the root's, or the root is a scalar or flow
collection) is the **document footer**, up to the document's `...`.
Between documents, when the next document exists it takes the comment
(rules 1–3) instead; after the last document (explicit `...` or not), the
comment is the last document's footer.

### Rule 8 — Blank lines
Every comment carries the number of **blank lines** (lines holding only
white space) between the previous non-blank line and its own line; a
same-line comment carries 0. Blank lines that belong to a block scalar's
text (kept by `|+`) are content, not separation, and are not counted.
The count travels with the comment when an edit moves it; the same count
for nodes is exposed with the comment API.

### Rule 9 — Flow collections
An own-line comment inside a flow collection (the innermost collection
holding both P and N is a flow collection) follows the same owner model
item by item, with no column conditions (indentation carries no meaning
in flow context):

- if the next content after it, `,` skipped, is a closing `]` or `}`, the
  comment is **that collection's footer**;
- otherwise it is a **leading comment of the next item or entry** (what
  that content opens, as in rule 4).

Same-line comments in flow context are rule 5 (e.g. `a, # c` trails `a`;
`[ # c` trails the collection's entry or item, or the root).

### Rule 10 — Shallow leading comments (refines rule 4)
An own-line comment inside the root, in block context, **left of N's
column**, that rule 6 does not claim, is still a **leading comment of what
N opens**.

*Choice:* rule 4 requires the column to be at least the node's
indentation but says nothing about a shallower comment above a node that
continues the same collection (`a:⏎  b: 1⏎# c⏎  d: 2`). Nothing closes at
the comment, so no footer can take it; the next node is the only owner
that keeps it in place. Its own rule number makes the case visible in
`explain_comment`.

### Rule 11 — Explicit entries: the `:` line
An own-line comment directly above the `:` of an explicit (`? `) entry in
block context is a **leading comment of that entry's value** (an empty
value included). In flow context rule 9 fires and gives the same owner.
Comments between `?` and its key on a later line lead the entry (rule 4,
the key names it).

### Rule 12 — Inside a node: properties on their own line
An own-line comment followed by content that **starts no node** — a
property after the node's first property (`&a⏎# c⏎!!map`), a block scalar
header on its own line, or error bytes — is a **leading comment of the
innermost node holding that content**. A comment above a node's *first*
property leads the node (rule 4: N starts it); a comment between a
collection's properties and its first entry leads that entry (rule 4).

### Rule 13 — Run order: owners only move outward
Within one **run** (the comment lines between one content token P and the
next, N), attachment is **monotonic**: every owner's comments are
contiguous, in source order, with the owner (edits move an owner's
comments as one range, which relies on this).

The owners an own-line comment of a run can get are **ranked from deep to
outer**: the run's closing block collections, innermost first (rule 6),
then everything past them — the next node's leading comment (rules 4, 10,
11, 12), the document footer (rule 7), the next document's header (rules
1, 2), a flow collection's item or footer (rule 9). Decide each comment of
the run in source order by rules 1–12; **when its owner ranks deeper than
the owner of an earlier comment of the run, it takes the owner of the
comment before it** (rule 13), and so does every later comment of the run
that would again go deeper. Once a run has reached an outer owner, no
later comment of the run returns to a deeper one.

Examples (goldens `r13_*`):

- `a:⏎  b:⏎    c: 1⏎  # c2⏎    # c4`: `# c2` is the footer of `b:`'s
  mapping (rule 6); `# c4` would be the footer of the deeper `c: 1`
  mapping, but that footer would not be contiguous with its mapping: it
  joins `# c2` (rule 13).
- `a:⏎  - 1⏎# lead b⏎  # - 2⏎b: 1`: `# lead b` leads `b` (rule 4);
  `# - 2` would be the list's footer, but lies between `# lead b` and `b`:
  it leads `b` too (rule 13).
- `a:⏎  - 1⏎# lead b⏎⏎  # - 2` (end of document): `# lead b` is the
  document footer (rule 7); `# - 2` joins it (rule 13).

Rules 2/3 at the top of a document are already in this order (header
comments come before the first node's leading comments), and rule 9 gives
a run one owner, so rule 13 changes only block-context footers.

## 4. Document context of an own-line comment

A document's children are its directives, `---`, the trivia around its
root, the root, what error recovery leaves after the root, and `...`. Let
D(P) and D(N) be the documents holding P and N, and XP and XN the
children of those documents holding them. For a token outside any
document (a stray `...`, or content of an error region at stream level),
D is the previous document for P and the next one for N — the documents
around the stream-level element holding the token.

- **Same document** (P and N both in D):
  - XN a directive or `---`: rule 1;
  - XP a directive or `---` (nothing of the root before the comment):
    rules 2/3 with XN as the root, or rule 2 when XN is not a node with
    bytes (e.g. the `...` of a document with no visible root);
  - XP = XN, a node: the comment is inside the root: rules 4, 6, 9, 10,
    11, 12;
  - otherwise (after the root: N is `...` or error bytes after the root):
    rule 6, else rule 7.
- **Between documents** (the comment is outside every document):
  - rule 1 when the next document begins with `---` or a directive;
  - else rule 6 for D(P) when P is in D(P) after its content (D(P) ended
    without `...`);
  - else, when a next document exists: rules 2/3 for it (rule 2 when N is
    a stray token before it);
  - else rule 2 for D(P) when P is its `---` or a directive (no content,
    no `...`);
  - else rule 7 for D(P);
  - else (the input has no document) rule 2, owner the stream.

Each case is decided from the kinds of P, N, XP, XN and of the next
document's first child, so no decision scans a document's children. The
passes (`SyntaxTree::comment_attachments`,
`SyntaxTree::document_comment_attachments`, and so `Document::comment_ids`
and `Document::explain_comments`) never scan the stream's elements per
comment either: they table the documents around each stream-level element
once (O(n)). The one-comment path (`SyntaxTree::comment_attachment`,
`Document::explain_comment`) reads P and N, the documents around a stray
`...` or a stream-level error region, and rule 13's deepest owner among the
earlier comments of its run from the syntax layer's comment-run index,
without walking the run or the stream's elements: its cost is
O(depth + log + its line), whatever the edits before. A comment
whose owner is a document header or the first node (rules 2/3) also reads
the blank lines after it in its run; columns beyond `u32::MAX / 2` and
recovered runs mixing header and footer owners are replayed (O(the run)).

## 5. Error recovery

Trees built from input with errors follow the same rules: an error region
(an `Error` node) is an item wherever it stands, and error bytes are
content tokens. Every comment still gets exactly one owner and nothing
panics; owners inside a recovered region are best effort and
are not covered by the stability promise. In particular:

- a same-line comment after error bytes skipped at document level is the
  document footer;
- where a recovered mapping's items cannot be paired from their
  neighbours (no `?`, `:`, `,` or bracket tells a key from a value), the
  item is read as a key: pairing is never decided by counting the
  mapping's items (that would cost more than linear time);
- a document can begin or end with trivia (the comments of an unclosed
  `[` cut off by the next `---`): its comments follow the rules like any
  other, and may belong to the neighbouring document;
- an owner is never the empty placeholder the parser puts right after an
  error region: the placeholder has no bytes and is the last child of its
  collection (everything after it on the following lines is skipped at
  document level), so no rule reaches it — owners are the nodes navigation
  presents, the error region itself included (checked by the syntax
  fuzz).

## 6. Choices and their reasons

The cases where the rules needed a decision, with the choice made and
why. The first table lists the cases where a simpler, literal reading of
the core rules would decide differently or leave a comment without an
owner.

### Where a literal reading falls short

| # | A literal reading says | This rulebook | Why |
|---|---|---|---|
| D1 | Rule 4: comment lines directly above a node, **no blank line between**, lead it. | Blank lines between the comment and the node do not matter (rule 4, or 10). | Read literally, a comment separated from the next node by a blank line inside a document (`a: 1⏎⏎# about b⏎⏎b: 2`) matches no rule: rules 1–3 are for the top, rule 6 needs a deeper column, rule 7 needs to be after the root. It would have no owner. Attaching it to the next node keeps it with the content it sits above when that content moves, and its blank lines travel with it as rule 8 counts. |
| D2 | Rule 4: leading only when the comment's column is **at least the node's indentation**. | A shallower comment that no footer claims still leads the next node, as rule 10. | The same gap as D1: `a:⏎  b: 1⏎# c⏎  d: 2` has no other owner (nothing closes at the comment). Rule 10's own number keeps the case visible in `explain_comment`. |
| D3 | Rule 3: the block becomes the **first node's** leading comment. | The innermost entry or item the root opens with (C1). | "First node" could also mean the root collection; a comment directly above `a: 1` is about the entry `a`. |
| D4 | Rules 6/7: an own-line comment after a collection's last child, **indented deeper than the next node at the parent level**, is that collection's footer; at root indentation, the document footer. | Rule 13 (run order): within one run, once a comment has gone to an outer owner, a later, deeper comment takes that outer owner instead of its rule-6 footer. So in `a:⏎  - 1⏎# lead b⏎  # - 2⏎b: 1` the commented-out item `# - 2`, deeper than `b`, leads `b` (golden `r13_footer_after_next_leading`); in `a:⏎  - 1⏎# lead b⏎⏎  # - 2` the column-2 `# - 2` is the document footer (golden `r13_footer_after_document_footer`). | Every owner's comments must be contiguous with it in source order, because edits move an owner's comments as one range. **Alternative considered (not implemented):** when a later comment of the run is deeper, pull the earlier, shallower comments inward instead, so commented-out items stay with their list (`# lead b` and `# - 2` would both be the list's footer). That also keeps owners contiguous, but a shallow comment then belongs to a collection it is outdented from. |

### Rejected alternative

- **R1** (rule 6 across documents: a deeper commented-out item before `---` stays the previous document's collection footer) was considered and rejected. Rule 1 stands: comments before `---` are always the next document's header.

### Recorded choices

| # | Case | Choice |
|---|---|---|
| C1 | Rule 3's "first node" | The innermost entry/item the root opens with (else the root); see D3. |
| C2 | Rule 3 column condition | Not applied at the top of a document. |
| C3 | Comment on the `---`/directive line, `...` line | Header / footer (rule 5). |
| C4 | Rule 1 vs 6/7 between documents | Rule 1 whenever a `---` follows; otherwise rule 6, then the next document's rules 2/3, then the previous document's footer (rule 7). |
| C5 | After `...` with no next document | Last document's footer (rule 7). |
| C6 | Rule 4 vs 6/7 after a block scalar | Middle entry: next entry's leading (rule 4/10). Last entry: footer (rule 6) or document footer (rule 7). |
| C7 | Shallower than the next node, nothing closes | Leading of the next node (rule 10); see D2. |
| C8 | Deeper than every closing collection | Outermost closing collection's footer (rule 6). |
| C9 | Explicit `? ` entries | `:` line comment → the value (rule 11); `?`-to-key comment → the entry. |
| C10 | Properties/headers on their own line | The node holding them (rule 12). |
| C11 | Flow context | No column rules; `,` skipped; before `]`/`}` → footer (rule 9). |
| C12 | Value on the line below its key (`k:⏎  # c⏎  v`) | Leading of the value node `v` (rule 4), not of the entry. |
| C13 | Blank lines inside `|+` text | Content, not counted (rule 8). |
| C14 | Input with no document | Header, owner the stream (rule 2). |
| C15 | Document with no visible root (`---` alone) | Its comments, and those after it when it ends without `...` and no document follows, are header (rule 2). |
| C16 | Stray `...` (or stream-level error region) between documents | Same line: next document's header, else previous document's footer, else the stream's header (rule 5). Own line before a stray token: the next document's header (rule 2), else the previous document's footer (rule 7). |
| C17 | Rules 2/3 after `---` | Applied after `---` too: a blank line before the root makes a header, none makes the first node's leading comment. |
| C18 | Rule 7 "at root indentation" | At or left of the root's column; after a scalar or flow root, any column (nothing there can have a footer). |
| C19 | `Document::parse_recovering` of several documents (one `Document` holding more than one) | It explains what it navigates: the first document's comments (the others through `Stream`); header and footer are the first document's. |
| C20 | Error recovery | Section 5: skipped bytes at document level → footer; unpaired recovered items read as keys; owners are the nodes navigation presents (never a recovery placeholder). |
