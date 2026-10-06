//! Shared checks for the `document_*` edit tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    dead_code
)]

use oneyaml::{Document, Value};

/// Both readers agree after an edit: the document's meaning equals the meaning
/// of its text, and its diagnostics equal those of parsing its text (strict for
/// a clean document, recovering for a restricted one), under its settings.
pub fn agrees_with_its_text(doc: &Document) {
    let text = doc.to_string();
    let s = doc.settings();
    let a = Value::try_from(doc);
    let b = oneyaml::from_str_with::<Value>(&text, s);
    match (&a, &b) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "meaning differs for {text:?}"),
        (Err(a), Err(b)) => assert_eq!(a.code(), b.code(), "{text:?}"),
        _ => panic!("tree {a:?} vs text {b:?} for {text:?}"),
    }
    if doc.has_errors() {
        let re = Document::parse_recovering_with(text.clone(), s);
        assert_eq!(re.value().diagnostics(), doc.diagnostics(), "{text:?}");
    } else {
        let re = Document::parse_with(text.clone(), s)
            .unwrap_or_else(|e| panic!("edited text does not parse: {e} in {text:?}"));
        assert_eq!(re.diagnostics(), doc.diagnostics(), "{text:?}");
        assert_eq!(re.to_string(), text);
    }
}
