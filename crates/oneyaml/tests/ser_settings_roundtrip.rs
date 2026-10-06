//! The round trip: output always reads back as written
//! **under the settings it was written with**. Every YAML 1.1 setting (and
//! merge keys, `honor_version_directive`, both presets) × both layouts ×
//! the scalar sweep and the awkward strings, each written as a document, a
//! sequence item, a mapping key, and a mapping value, must read back as the
//! same string with the same settings. A setting can only make the writer
//! quote more; under the default settings nothing changes (the goldens).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod ser_common;

use std::collections::BTreeMap;

use oneyaml::{Compat, Emit, Layout, Settings, Value};
use ser_common::{AWKWARD, scalar_sweep};

fn reading_settings() -> Vec<(&'static str, Settings)> {
    vec![
        ("default", Settings::new()),
        ("merge_keys", Settings::new().merge_keys(true)),
        ("yaml11_booleans", Settings::new().yaml11_booleans(true)),
        ("legacy_octal", Settings::new().legacy_octal(true)),
        ("sexagesimal", Settings::new().sexagesimal(true)),
        ("digit_separators", Settings::new().digit_separators(true)),
        ("yaml11_int_forms", Settings::new().yaml11_int_forms(true)),
        (
            "honor_version_directive",
            Settings::new().honor_version_directive(true),
        ),
        ("Compat::yaml_1_1", Compat::yaml_1_1()),
        ("Compat::serde_yaml", Compat::serde_yaml()),
    ]
}

fn layouts(s: &Settings) -> [(&'static str, Settings); 2] {
    let default = s.clone().emit(Emit::new());
    let serde_yaml = s.clone().emit(
        Emit::new()
            .layout(Layout::SerdeYaml)
            .indent_sequences(false),
    );
    [("Default", default), ("SerdeYaml", serde_yaml)]
}

fn texts() -> Vec<String> {
    let mut t = scalar_sweep();
    t.extend(AWKWARD.iter().map(|s| (*s).to_owned()));
    t
}

#[test]
fn every_setting_and_layout_reads_its_own_strings_back() {
    let texts = texts();
    assert!(texts.len() > 450, "{} texts", texts.len());
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for (sname, reading) in reading_settings() {
        for (lname, settings) in layouts(&reading) {
            for t in &texts {
                let doc = Value::from(t.as_str());
                let seq = Value::from(vec![Value::from(t.as_str())]);
                let map_key: BTreeMap<&str, i32> = BTreeMap::from([(t.as_str(), 1)]);
                let map_value: BTreeMap<&str, &str> = BTreeMap::from([("k", t.as_str())]);
                let outputs = [
                    ("document", oneyaml::to_string_with(&doc, &settings)),
                    ("item", oneyaml::to_string_with(&seq, &settings)),
                    ("key", oneyaml::to_string_with(&map_key, &settings)),
                    ("value", oneyaml::to_string_with(&map_value, &settings)),
                ];
                let expected = [
                    doc.clone(),
                    seq.clone(),
                    oneyaml::to_value(&map_key).unwrap(),
                    oneyaml::to_value(&map_value).unwrap(),
                ];
                for ((place, out), want) in outputs.into_iter().zip(expected) {
                    checked = checked.saturating_add(1);
                    let out = out.unwrap_or_else(|e| panic!("{sname}/{lname} {t:?} {place}: {e}"));
                    match oneyaml::from_str_with::<Value>(&out, &reading) {
                        Ok(back) if back == want => {}
                        other => failures.push(format!(
                            "{sname}/{lname} {t:?} as {place}: wrote {out:?}, read {other:?}"
                        )),
                    }
                }
            }
        }
    }
    assert!(checked > 30_000, "{checked}");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Known repros.
#[test]
fn serde_yaml_layout_quotes_what_the_active_settings_would_retype() {
    let cases = [
        ("no", Compat::serde_yaml().yaml11_booleans(true)),
        ("12:30", Compat::serde_yaml().sexagesimal(true)),
        ("0644", Compat::serde_yaml().legacy_octal(true)),
        ("1_000", Compat::serde_yaml().digit_separators(true)),
    ];
    for (text, settings) in cases {
        let out = oneyaml::to_string_with(&text, &settings).unwrap();
        assert!(out.starts_with(['\'', '"']), "{text:?} -> {out:?}");
        let back: String = oneyaml::from_str_with(&out, &settings).unwrap();
        assert_eq!(back, text);
    }
    // A plain `<<` key would merge under merge keys.
    let m = BTreeMap::from([("<<", 1)]);
    for settings in [
        Compat::serde_yaml().merge_keys(true),
        Settings::new().merge_keys(true),
    ] {
        let out = oneyaml::to_string_with(&m, &settings).unwrap();
        assert!(out.starts_with(['\'', '"']), "{out:?}");
        let back: BTreeMap<String, i32> = oneyaml::from_str_with(&out, &settings).unwrap();
        assert_eq!(back["<<"], 1);
    }
    // serde_yaml reads a signed `0o` as an integer, so its layout quotes it.
    for text in ["-0o17", "+0o17", "-0o1", "+0o1"] {
        let out = oneyaml::to_string_with(&text, &Compat::serde_yaml()).unwrap();
        assert_eq!(out, format!("'{text}'\n"));
    }
}
