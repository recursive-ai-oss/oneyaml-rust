//! Conflicting settings: a combination that makes a setting
//! irrelevant is a **warning**; one that would produce incorrect results is
//! an **error**. Never a panic, never a silent no-op.
//!
//! This reports *which* combinations conflict; the diagnostic codes and the
//! point where they are raised (parse / serialize entry) come with the
//! layers that consume settings.

use oneyaml_core::{Schema, Settings};

/// How serious a conflict is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConflictSeverity {
    /// Meaningless: a setting has no effect.
    Warning,
    /// Incorrect: output would not read back as the same value.
    Error,
}

/// One conflicting combination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Conflict {
    /// A YAML 1.1 scalar behaviour (`yaml11_booleans`, `legacy_octal`,
    /// `sexagesimal`, `digit_separators`, `yaml11_int_forms`) with the JSON
    /// or Failsafe schema.
    /// These behaviours extend the Core schema only, so they do nothing.
    Yaml11WithoutCore {
        /// The setting's name, as in `Settings`.
        setting: &'static str,
        /// The active schema.
        schema: Schema,
    },
    /// `honor_version_directive` with the JSON or Failsafe schema: honouring
    /// `%YAML 1.1` only switches on the 1.1 scalar behaviours, which the
    /// schema ignores. (Merge keys still switch on, so this is reported
    /// only when `merge_keys` is already on too.)
    HonorDirectiveWithoutCore {
        /// The active schema.
        schema: Schema,
    },
    /// `honor_version_directive` with every YAML 1.1 behaviour already on:
    /// a `%YAML 1.1` directive can change nothing.
    HonorDirectiveRedundant,
    /// `Emit::indent(0)`: nested block collections cannot be expressed
    /// without indentation, so output would not read back.
    ZeroIndent,
}

impl Conflict {
    /// Warning or error.
    pub(crate) fn severity(self) -> ConflictSeverity {
        match self {
            Conflict::Yaml11WithoutCore { .. }
            | Conflict::HonorDirectiveWithoutCore { .. }
            | Conflict::HonorDirectiveRedundant => ConflictSeverity::Warning,
            Conflict::ZeroIndent => ConflictSeverity::Error,
        }
    }
}

/// Every conflict in `settings`, in a fixed order.
pub(crate) fn check_settings(settings: &Settings) -> Vec<Conflict> {
    let mut out = Vec::new();
    let schema = settings.get_schema();
    let non_core = matches!(schema, Schema::Json | Schema::Failsafe);
    let yaml11 = [
        ("yaml11_booleans", settings.get_yaml11_booleans()),
        ("legacy_octal", settings.get_legacy_octal()),
        ("sexagesimal", settings.get_sexagesimal()),
        ("digit_separators", settings.get_digit_separators()),
        ("yaml11_int_forms", settings.get_yaml11_int_forms()),
    ];
    if non_core {
        out.extend(
            yaml11
                .iter()
                .filter(|(_, on)| *on)
                .map(|(setting, _)| Conflict::Yaml11WithoutCore { setting, schema }),
        );
    }
    if settings.get_honor_version_directive() {
        let all_scalar = yaml11.iter().all(|(_, on)| *on);
        if settings.get_merge_keys() && (non_core || all_scalar) {
            out.push(if non_core {
                Conflict::HonorDirectiveWithoutCore { schema }
            } else {
                Conflict::HonorDirectiveRedundant
            });
        }
    }
    if settings.get_emit().get_indent() == 0 {
        out.push(Conflict::ZeroIndent);
    }
    out
}
