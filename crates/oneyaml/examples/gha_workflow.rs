//! Real-world example: editing a GitHub Actions workflow
//! without losing its comments.
//!
//! The edits:
//!
//! 1. insert a toolchain step *above* a commented step: the comment stays
//!    with the step it describes (rule 4 gives it to the step below it);
//! 2. move the audit step, with its comment, from `test` to `lint`;
//! 3. reorder the jobs (`test` first); each job keeps its comment, and the
//!    one that no longer holds (`# Lints run first`) is cleared; the blank
//!    line that moved with `test`'s comment is put back between the jobs;
//! 4. drop the redundant `cargo build` step with `remove_keep_comments`,
//!    so the `# Build, then test.` note stays on the next step;
//! 5. delete the deprecated `nightly` job (its comment goes with it, in
//!    the returned `Removed`) and leave a pointer as the footer of `jobs`;
//! 6. bump `actions/checkout` in both jobs and pin `lint`'s runner;
//! 7. add a manual trigger (`workflow_dispatch`), written empty like the
//!    other events.
//!
//! Run with `cargo run -p oneyaml --example gha_workflow`; `cargo test -p
//! oneyaml` runs it as a test.

mod common;

use oneyaml::{CommentOwner, Document, NodeKind, Value, path};
use serde::Serialize;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const WORKFLOW: &str = "\
# CI for the web service.

name: ci

on:
  push:
    branches: [main]
  pull_request:

jobs:
  # Lints run first: they are the cheapest.
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo clippy --all-targets -- -D warnings

  # The test matrix.
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      # Caches ~/.cargo and target/ between runs.
      - uses: Swatinem/rust-cache@v2
      # Advisories and licenses.
      - run: cargo deny check
      # Build, then test.
      - run: cargo build --workspace
      - run: cargo test --workspace

  # Deprecated: the nightly job moved to nightly.yml.
  nightly:
    runs-on: ubuntu-latest
    steps:
      - run: cargo +nightly test
";

/// The workflow after [`edit`].
const EDITED: &str = "\
# CI for the web service.

name: ci

on:
  push:
    branches: [main]
  pull_request:
  workflow_dispatch:

jobs:
  # The test matrix.
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      # Caches ~/.cargo and target/ between runs.
      - uses: Swatinem/rust-cache@v2
      # Build, then test.
      - run: cargo test --workspace

  lint:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v5
      # Advisories and licenses.
      - run: cargo deny check
      - run: cargo clippy --all-targets -- -D warnings
  # nightly: see nightly.yml
";

const OWNERS_BEFORE: &[&str] = &[
    "# CI for the web service. -> document header (rule 2)",
    "# Lints run first: they are the cheapest. -> leading of `lint` (rule 4)",
    "# The test matrix. -> leading of `test` (rule 4)",
    "# Caches ~/.cargo and target/ between runs. -> leading of `uses: Swatinem/rust-cache@v2` (rule 4)",
    "# Advisories and licenses. -> leading of `run: cargo deny check` (rule 4)",
    "# Build, then test. -> leading of `run: cargo build --workspace` (rule 4)",
    "# Deprecated: the nightly job moved to nightly.yml. -> leading of `nightly` (rule 4)",
];

/// Every comment that stayed keeps its kind of owner; the build step's
/// note now leads the test step (`remove_keep_comments`), and the new
/// pointer is the footer of `jobs` (rule 6).
const OWNERS_AFTER: &[&str] = &[
    "# CI for the web service. -> document header (rule 2)",
    "# The test matrix. -> leading of `test` (rule 4)",
    "# Caches ~/.cargo and target/ between runs. -> leading of `uses: Swatinem/rust-cache@v2` (rule 4)",
    "# Build, then test. -> leading of `run: cargo test --workspace` (rule 4)",
    "# Advisories and licenses. -> leading of `run: cargo deny check` (rule 4)",
    "# nightly: see nightly.yml -> footer of `jobs:` value (rule 6)",
];

/// A step that runs an action.
#[derive(Serialize)]
struct Uses {
    uses: &'static str,
}

const CHECKOUT_V5: Uses = Uses {
    uses: "actions/checkout@v5",
};

fn edit() -> Result<Document> {
    let mut doc = Document::parse(WORKFLOW)?;
    assert_eq!(common::owners(&doc)?, OWNERS_BEFORE);

    // 1. A new step at index 1, above the commented cache step. The new
    //    step brings no comment; the cache step keeps its own.
    let test_steps = path!["jobs", "test", "steps"];
    let cache = doc
        .id_of(&path!["jobs", "test", "steps", 1])
        .ok_or("no cache step")?;
    doc.get_path_mut(&test_steps)?.insert_at(
        1,
        &Uses {
            uses: "dtolnay/rust-toolchain@stable",
        },
    )?;
    assert_eq!(
        doc.comments(cache)?.leading().first().map(|c| c.text()),
        Some("Caches ~/.cargo and target/ between runs.")
    );

    // 2. Move the audit step (index 3 now) to `lint`, before clippy. The
    //    `Removed` carries the comment the rulebook gives the step.
    let audit = doc.get_path_mut(&test_steps)?.remove_at(3)?;
    assert_eq!(audit.kind(), NodeKind::Mapping);
    assert_eq!(audit.comments(), ["# Advisories and licenses."]);
    doc.get_path_mut(&path!["jobs", "lint", "steps"])?
        .insert_removed_at(1, audit)?;

    // 3. `test` first. Its comment line has a blank line above it, which
    //    moves with it (see `EDITED`).
    let test_key = doc
        .get_path(&path!["jobs", "test"])
        .and_then(|n| n.key())
        .ok_or("test has no key")?;
    let leading = test_key.comments()?;
    let blank = leading.leading().first().map(|c| c.blank_lines_before());
    assert_eq!(blank, Some(1));
    assert_eq!(leading.blank_lines_before(), 0);
    doc.get_mut("jobs")?.move_entry(1, 0)?;
    // `lint` no longer runs first: clear its comment (handed back). An
    // entry's comments belong to its key (`into_key`).
    let mut lint = doc.get_path_mut(&path!["jobs", "lint"])?.into_key()?;
    assert_eq!(
        lint.clear_comments()?,
        ["# Lints run first: they are the cheapest."]
    );
    let lint_key = lint.id();
    // The blank line above `test`'s comment moved with it (rule 8: blank
    // lines travel with what follows them), so it now sits right below
    // `jobs:`; put it back between the jobs.
    // (A moved entry has new handles: look `test` up again.)
    let test_key = doc
        .get_path(&path!["jobs", "test"])
        .and_then(|n| n.key())
        .ok_or("test has no key")?
        .id();
    let test_note = doc
        .comments(test_key)?
        .leading()
        .first()
        .map(|c| c.id())
        .ok_or("test has no comment")?;
    doc.set_comment_blank_lines(test_note, 0)?;
    doc.node_mut(lint_key)?.set_blank_lines_before(1)?;
    assert_eq!(doc.explain_comment(test_note)?.blank_lines_before(), 0);
    assert_eq!(doc.comments(lint_key)?.blank_lines_before(), 1);

    // 4. `cargo test` builds anyway: drop the build step but keep its note,
    //    which now leads the next step.
    let build = doc
        .id_of(&path!["jobs", "test", "steps", 3])
        .ok_or("no build step")?;
    let removed = doc.remove_keep_comments(build)?;
    assert_eq!(removed.text(), "run: cargo build --workspace");
    assert!(removed.comments().is_empty());

    // 5. Delete the nightly job. Its comment is its own (rule 4), so it
    //    goes with it; `Removed::comments` lists what left (nothing
    //    is destroyed silently). Then leave a pointer in `jobs`' footer.
    let nightly = doc.remove_path(&path!["jobs", "nightly"])?;
    assert_eq!(nightly.key(), Some("nightly"));
    assert_eq!(
        nightly.comments(),
        ["# Deprecated: the nightly job moved to nightly.yml."]
    );
    let mut jobs = doc.get_mut("jobs")?;
    let pointer = jobs.push_footer("nightly: see nightly.yml")?;
    let jobs = jobs.id();
    assert_eq!(
        doc.explain_comment(pointer)?.owner(),
        CommentOwner::Footer(jobs)
    );

    // 6. Bump checkout (a whole step replaced, by index or through a
    //    handle), and pin `lint`'s runner (`set_scalar` keeps the style).
    doc.get_path_mut(&path!["jobs", "lint", "steps"])?
        .replace_at(0, &CHECKOUT_V5)?;
    doc.get_path_mut(&test_steps)?
        .get_index_mut(0)?
        .set(&CHECKOUT_V5)?;
    doc.get_path_mut(&path!["jobs", "lint", "runs-on"])?
        .set_scalar("ubuntu-24.04")?;

    // 7. A manual trigger: a new null event entry, written empty like its
    //    neighbours (`pull_request:`).
    doc.get_path_or_create(&path!["on", "workflow_dispatch"])?;

    assert_eq!(common::owners(&doc)?, OWNERS_AFTER);
    Ok(doc)
}

fn run() -> Result<String> {
    // The untouched input round-trips byte for byte.
    assert_eq!(
        Document::parse(WORKFLOW)?.to_string().as_bytes(),
        WORKFLOW.as_bytes()
    );

    let doc = edit()?;
    let text = doc.to_string();
    assert_eq!(text, EDITED);

    // Serde reads the edited text as the `Document` means it.
    let from_serde: Value = oneyaml::from_str(&text)?;
    assert_eq!(from_serde, doc.to_value()?);
    // YAML 1.2: `on` is the string "on" (a boolean only to 1.1 readers).
    // (`get` rather than the never-panicking `Index`, which clippy's
    // `indexing_slicing` lint flags all the same.)
    let jobs: Vec<_> = from_serde
        .get("jobs")
        .and_then(Value::as_mapping)
        .ok_or("jobs is not a mapping")?
        .keys()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(jobs, ["test", "lint"]);
    let dispatch = from_serde
        .get("on")
        .and_then(|on| on.get("workflow_dispatch"));
    assert!(dispatch.is_some_and(Value::is_null));
    Ok(text)
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> Result<()> {
    print!("{}", run()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn gha_workflow() -> super::Result<()> {
        super::run().map(|_| ())
    }
}
