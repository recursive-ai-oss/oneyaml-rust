//! What the build script records of the settings a `bench` build was made
//! with, beyond its profile's name: shared by `build.rs` and the tool's
//! tests.

/// The notes for a build: an optimization level other than 3, every
/// `CARGO_PROFILE_*` variable in the build's environment (of any profile:
/// `--profile bench` builds into the `release` directory with the `bench`
/// profile's variables, and a variable of an unrelated profile is refused
/// too rather than judged harmless), and extra compiler flags
/// (`CARGO_ENCODED_RUSTFLAGS`). Empty when there is none of these.
pub(crate) fn notes(
    opt_level: Option<&str>,
    vars: impl IntoIterator<Item = (String, String)>,
    encoded_rustflags: &str,
) -> String {
    let mut notes = Vec::new();
    match opt_level {
        Some("3") => {}
        Some(level) => notes.push(format!("opt-level {level}")),
        None => notes.push("opt-level unknown".to_owned()),
    }
    let mut overrides: Vec<String> = vars
        .into_iter()
        .filter(|(k, _)| k.starts_with("CARGO_PROFILE_"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    overrides.sort();
    if !overrides.is_empty() {
        notes.push(format!("overridden: {}", overrides.join(" ")));
    }
    let flags: Vec<&str> = encoded_rustflags
        .split('\u{1f}')
        .filter(|f| !f.is_empty())
        .collect();
    if !flags.is_empty() {
        notes.push(format!("compiler flags: {}", flags.join(" ")));
    }
    // One line: `|` and newlines would break the table cell.
    notes.join("; ").replace(['|', '\n', '\r'], " ")
}
