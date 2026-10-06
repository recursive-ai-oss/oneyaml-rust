//! Conflicting settings as diagnostics: meaningless → warning
//! ([`code::MEANINGLESS_SETTINGS`]), incorrect → error
//! ([`code::INCORRECT_SETTINGS`]).

use oneyaml_core::{Diagnostic, Label, Schema, Settings, Span, code};

use crate::error::Error;
use crate::resolve::conflict::{Conflict, ConflictSeverity, check_settings};

fn schema_name(s: Schema) -> &'static str {
    match s {
        Schema::Json => "JSON",
        Schema::Failsafe => "Failsafe",
        _ => "Core",
    }
}

/// The diagnostic for one conflict. Settings have no source position, so
/// the label is empty at offset 0.
pub(crate) fn conflict_diagnostic(c: Conflict) -> Diagnostic {
    let (message, help) = match c {
        Conflict::Yaml11WithoutCore { setting, schema } => (
            format!(
                "`{setting}` has no effect with the {} schema",
                schema_name(schema)
            ),
            "YAML 1.1 scalar behaviours extend the Core schema only; use Schema::Core or turn the setting off".to_owned(),
        ),
        Conflict::HonorDirectiveWithoutCore { schema } => (
            format!(
                "`honor_version_directive` has no effect with the {} schema and merge keys on",
                schema_name(schema)
            ),
            "a %YAML 1.1 directive only switches on Core-schema behaviours and merge keys".to_owned(),
        ),
        Conflict::HonorDirectiveRedundant => (
            "`honor_version_directive` has no effect: every YAML 1.1 behaviour is already on"
                .to_owned(),
            "turn honor_version_directive off, or the 1.1 settings off".to_owned(),
        ),
        Conflict::ZeroIndent => (
            "`Emit::indent(0)` cannot express nested block collections".to_owned(),
            "use an indent of at least 1".to_owned(),
        ),
    };
    let label = Label::new(Span::empty(0), "");
    let d = match c.severity() {
        ConflictSeverity::Warning => {
            Diagnostic::warning(code::MEANINGLESS_SETTINGS, message, label)
        }
        ConflictSeverity::Error => Diagnostic::error(code::INCORRECT_SETTINGS, message, label),
    };
    d.with_help(help)
}

/// Whether a conflict concerns reading (the read path ignores output-only
/// conflicts; the serializer reports them).
fn affects_reading(c: Conflict) -> bool {
    !matches!(c, Conflict::ZeroIndent)
}

/// The settings diagnostics of the read path: warnings to record, or the
/// first incorrect combination as an error.
pub(crate) fn read_diagnostics(settings: &Settings) -> Result<Vec<Diagnostic>, Error> {
    diagnostics(settings, affects_reading)
}

/// Settings diagnostics for the conflicts `relevant` selects.
pub(crate) fn diagnostics(
    settings: &Settings,
    relevant: fn(Conflict) -> bool,
) -> Result<Vec<Diagnostic>, Error> {
    let mut warnings = Vec::new();
    for c in check_settings(settings)
        .into_iter()
        .filter(|c| relevant(*c))
    {
        let d = conflict_diagnostic(c);
        match c.severity() {
            ConflictSeverity::Error => return Err(Error::from_diagnostic(d)),
            ConflictSeverity::Warning => warnings.push(d),
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use oneyaml_core::{Compat, Emit, Severity};

    #[test]
    fn warnings_and_errors() {
        assert!(read_diagnostics(&Settings::new()).unwrap().is_empty());
        let w =
            read_diagnostics(&Settings::new().schema(Schema::Json).yaml11_int_forms(true)).unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].code, code::MEANINGLESS_SETTINGS);
        assert_eq!(w[0].severity, Severity::Warning);
        assert!(w[0].message.contains("yaml11_int_forms"));
        let w = read_diagnostics(&Compat::yaml_1_1().honor_version_directive(true)).unwrap();
        assert!(w[0].message.contains("already on"));
        let w = read_diagnostics(
            &Settings::new()
                .honor_version_directive(true)
                .merge_keys(true)
                .schema(Schema::Failsafe),
        )
        .unwrap();
        assert!(w[0].message.contains("Failsafe"));
        // Output-only conflicts do not concern reading ...
        let zero = Settings::new().emit(Emit::new().indent(0));
        assert!(read_diagnostics(&zero).unwrap().is_empty());
        // ... but are errors where they apply (the serializer).
        let e = diagnostics(&zero, |_| true).unwrap_err();
        assert_eq!(e.code(), code::INCORRECT_SETTINGS);
        assert_eq!(e.kind(), crate::ErrorKind::Schema);
    }
}
