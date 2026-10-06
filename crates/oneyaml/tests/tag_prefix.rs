//! YAML 1.2.2 §6.8.2.2: a `%TAG` prefix's `%XX`
//! escapes are decoded with the suffix, as one URI, in every reader: the
//! serde path, `Value`, `Document` (`NodeRef::tag`), and the resolver; the
//! emitter writes the decoded tag so that it reads back to the same URI.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{Document, EditError, Tag, TaggedValue, Value, code, from_str, path, to_string};

fn tagged(tag: &str, v: Value) -> Value {
    Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new(tag),
        value: v,
    }))
}

/// The tag every reader gives the root of `src`.
fn root_tag(src: &str) -> String {
    let v: Value = from_str(src).unwrap();
    let from_value = v.as_tagged().map(|t| t.tag.to_string());
    let doc = Document::parse(src).unwrap();
    let from_doc = doc.root().unwrap().tag().unwrap().map(|t| t.to_string());
    // `Value::from(Document) == from_str::<Value>()`.
    assert_eq!(doc.to_value().unwrap(), v, "{src:?}");
    assert_eq!(from_value, from_doc, "{src:?}");
    from_doc.expect("a tag")
}

#[test]
fn the_three_spellings_of_one_uri_agree() {
    let spellings = [
        "%TAG !e! tag:x,1:%21\n--- !e!a v\n",
        "--- !<tag:x,1:%21a> v\n",
        "%TAG !e! tag:x,1:\n--- !e!%21a v\n",
    ];
    let want = tagged("tag:x,1:!a", Value::from("v"));
    for src in spellings {
        assert_eq!(root_tag(src), "tag:x,1:!a", "{src:?}");
        assert_eq!(from_str::<Value>(src).unwrap(), want, "{src:?}");
    }
    // Split multibyte escapes, as `é` written whole.
    let e = tagged("tag:x,1:é", Value::from("a"));
    for src in [
        "%TAG !e! tag:x,1:%C3\n--- !e!%A9 a\n",
        "%TAG !e! tag:x,1:\n--- !e!%C3%A9 a\n",
        "--- !<tag:x,1:%C3%A9> a\n",
    ] {
        assert_eq!(from_str::<Value>(src).unwrap(), e, "{src:?}");
    }
}

/// The emitter writes the decoded tag verbatim, re-escaping what a
/// tag cannot hold; it reads back as the same URI (once, an
/// escaped prefix gave `tag:x,1:%21a`, written `!<tag:x,1:%2521a>`).
#[test]
fn emitted_tags_read_back_to_the_same_uri() {
    for (src, written) in [
        ("%TAG !e! tag:x,1:%21\n--- !e!a v\n", "!<tag:x,1:!a> v\n"),
        (
            "%TAG !e! tag:x,1:%C3\n--- !e!%A9 a\n",
            "!<tag:x,1:%C3%A9> a\n",
        ),
        (
            "%TAG !e! tag:x,1:%25\n--- !e!41 a\n",
            "!<tag:x,1:%2541> a\n",
        ),
        ("%TAG !e! tag:x,1:%20\n--- !e!b a\n", "!<tag:x,1:%20b> a\n"),
        ("%TAG ! !%21\n--- !x a\n", "!%21x a\n"),
    ] {
        let v: Value = from_str(src).unwrap();
        let out = to_string(&v).unwrap();
        assert_eq!(out, written, "{src:?}");
        assert_eq!(from_str::<Value>(&out).unwrap(), v, "{src:?}");
    }
}

#[test]
fn invalid_utf8_after_decoding_is_y0013_everywhere() {
    for src in [
        "%TAG !e! %FF\n--- !e!x a\n",
        "%TAG !e! tag:x,1:%C3\n--- !e!x a\n",
        "%TAG ! !%FF\n--- !x a\n",
        "%TAG !! tag:yaml.org,2002:%FF\n--- !!str a\n",
        "!a%FF x\n",
    ] {
        assert_eq!(
            from_str::<Value>(src).unwrap_err().code(),
            code::INVALID_TAG,
            "{src:?}"
        );
        assert_eq!(
            Document::parse(src).unwrap_err().code(),
            code::INVALID_TAG,
            "{src:?}"
        );
        // Recovering: the document keeps its bytes and reports it.
        let parsed = Document::parse_recovering(src);
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|d| d.code == code::INVALID_TAG),
            "{src:?}"
        );
        assert_eq!(parsed.value().to_string().as_bytes(), src.as_bytes());
    }
}

/// The resolver: a `!!` handle redeclared with an escaped Core prefix
/// names the Core tags (the same URI as the default `!!` and the verbatim
/// form); escapes that change the URI make it a custom tag.
#[test]
fn escaped_secondary_handle_overrides_resolve_as_their_uri() {
    for src in [
        "%TAG !! tag:yaml.org%2C2002:\n--- !!int 12\n",
        "%TAG !! tag%3Ayaml.org,2002:\n--- !!int 12\n",
        "--- !!int 12\n",
        "--- !<tag:yaml.org%2C2002:int> 12\n",
    ] {
        assert_eq!(from_str::<Value>(src).unwrap(), Value::from(12), "{src:?}");
        assert_eq!(from_str::<u8>(src).unwrap(), 12, "{src:?}");
    }
    // `!!str` under an escaped Core prefix: a string, as the default.
    let v: Value = from_str("%TAG !! tag:yaml.org%2C2002:\n--- !!str 12\n").unwrap();
    assert_eq!(v, Value::from("12"));
    // An escape that changes the URI is another tag.
    let v: Value = from_str("%TAG !! tag:yaml.org,2002:%21\n--- !!int 12\n").unwrap();
    assert_eq!(v, tagged("tag:yaml.org,2002:!int", Value::from("12")));
}

/// A known limitation, kept: the edit layer still refuses writing a tag
/// into a document with `%TAG` directives (here, moving an entry whose tag
/// uses an escaped prefix), and reads such tags like every other reader.
#[test]
fn the_edit_layer_keeps_refusing_tags_in_tag_directive_documents() {
    let src = "%TAG !e! tag:x,1:%21\n---\n- !e!a foo\n- b\n";
    let mut d = Document::parse(src).unwrap();
    assert_eq!(
        d.get_path(&path![0]).unwrap().tag().unwrap().unwrap(),
        "tag:x,1:!a"
    );
    let r = d.remove_path(&path![0]).unwrap();
    let after = d.to_string();
    let e = d.root_mut().unwrap().insert_removed(r).unwrap_err();
    assert!(
        matches!(
            e.error(),
            EditError::Unsupported { reason, .. }
                if *reason == "a tag written into a document with %TAG directives"
        ),
        "{e:?}"
    );
    assert_eq!(d.to_string(), after);
    // Untagged edits still work, and the tag read stays the decoded one.
    let src = "%TAG !e! tag:x,1:%21\n---\na: !e!a foo\nb: 1\n";
    let mut d = Document::parse(src).unwrap();
    d.get_mut("b").unwrap().set(2).unwrap();
    assert_eq!(
        d.to_string(),
        "%TAG !e! tag:x,1:%21\n---\na: !e!a foo\nb: 2\n"
    );
    assert_eq!(
        d.get_path(&path!["a"]).unwrap().tag().unwrap().unwrap(),
        "tag:x,1:!a"
    );
}
