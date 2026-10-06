//! Every-reader check: other YAML libraries read oneyaml's **default**
//! output as the same data our own `from_str` does.
//!
//! For every golden case in the default layout and every property-corpus
//! value, the serde comparators (serde_yaml_ng, serde_norway, serde-saphyr)
//! read our output, and their `serde_json::Value` must equal ours wherever
//! the value is JSON-representable (string keys, no tags, finite floats,
//! integers within `i64`/`u64`). A plain scalar any of them reads as a
//! non-string is a failure of the quoting rule.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

#[path = "../../oneyaml/tests/ser_common/mod.rs"]
mod ser_common;

use oneyaml::Value;
use oracles::serde_cmp::{Lib, Opts, value};

/// Whether `v` means the same thing as JSON (iterative, no recursion).
fn json_representable(v: &Value) -> bool {
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::Tagged(_) => return false,
            Value::Number(n) => {
                let fits = n.as_i64().is_some() || n.as_u64().is_some();
                let finite = n.as_float().is_some_and(f64::is_finite);
                if !(fits || finite) {
                    return false;
                }
            }
            Value::Sequence(s) => stack.extend(s.iter()),
            Value::Mapping(m) => {
                for (k, v) in m {
                    if !k.is_string() {
                        return false;
                    }
                    stack.push(v);
                }
            }
            _ => {}
        }
    }
    true
}

fn check(name: &str, out: &str, failures: &mut Vec<String>) -> bool {
    let ours: Value = oneyaml::from_str(out).unwrap_or_else(|e| panic!("{name}: {e}"));
    // serde_json (like every serde format) recurses per level: stay shallow.
    if !json_representable(&ours) || ours.depth() > 100 {
        return false;
    }
    let want = serde_json::to_value(&ours).unwrap();
    for lib in [Lib::YamlNg, Lib::Norway, Lib::Saphyr] {
        match value(lib, out, Opts::default()).map(|d| d.to_json()) {
            Ok(Ok(docs)) if docs.len() == 1 && docs[0] == want => {}
            other => failures.push(format!(
                "{name}: {} read\n{out}as {other:?}\nwant {want}",
                lib.name()
            )),
        }
    }
    true
}

#[test]
fn golden_outputs_read_the_same_everywhere() {
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for case in ser_common::cases() {
        let (true, Ok(out)) = (case.default_layout, &case.output) else {
            continue;
        };
        // Streams hold several documents (and tags); single documents only.
        if case.name.contains("stream") {
            continue;
        }
        if check(&case.name, out, &mut failures) {
            checked += 1;
        }
    }
    assert!(
        checked >= 30,
        "only {checked} golden cases were JSON-representable"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn property_outputs_read_the_same_everywhere() {
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for (i, v) in ser_common::property_values(600).iter().enumerate() {
        let out = oneyaml::to_string(v).unwrap();
        if check(&format!("#{i}"), &out, &mut failures) {
            checked += 1;
        }
    }
    // Every awkward string on its own, and as a mapping key.
    for (i, s) in ser_common::AWKWARD.iter().enumerate() {
        let mut m = oneyaml::Mapping::new();
        m.insert(Value::from(*s), Value::from(*s));
        for v in [
            Value::from(*s),
            Value::from(m),
            Value::from(vec![Value::from(*s)]),
        ] {
            let out = oneyaml::to_string(&v).unwrap();
            if check(&format!("awkward #{i} {s:?}"), &out, &mut failures) {
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 500,
        "only {checked} property values were JSON-representable"
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
