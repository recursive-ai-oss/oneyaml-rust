//! Pinned decisions: tagged scalars into strings, the per-document
//! alias budget, `Serialize for Value` depth, and the
//! reader's encoding-aware size cap.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{
    Deserializer, Limits, Settings, Value, code, from_reader_with, from_str, from_str_with,
};
use serde::Deserialize;

#[test]
fn explicit_core_tags_decide_the_type() {
    // An explicit core tag decides the type per spec, for
    // quoted and plain scalars alike.
    assert_eq!(from_str::<u16>("!!int '123'").unwrap(), 123);
    assert_eq!(from_str::<u16>("!!int \"123\"").unwrap(), 123);
    let e = from_str::<String>("!!int 8080").unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    // Untagged: plain text into strings, quoted never into numbers.
    assert_eq!(from_str::<String>("8080").unwrap(), "8080");
    assert_eq!(
        from_str::<u16>("'123'").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    // In a struct, with the path.
    #[derive(Debug, Deserialize)]
    struct Port {
        #[allow(dead_code)]
        port: String,
    }
    let e = from_str::<Port>("port: !!int 8080").unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    assert_eq!(e.path().unwrap().to_string(), "port");
}

/// One document whose aliases expand to about 200k nodes (within the
/// default budget of max(10 × own nodes, 1M)).
fn heavy_document() -> String {
    let items = vec!["x"; 1000].join(", ");
    let refs = vec!["*a"; 200].join(", ");
    format!("a: &a [{items}]\nb: [{refs}]\n")
}

#[test]
fn alias_budget_is_per_document() {
    let doc = heavy_document();
    assert!(from_str::<Value>(&doc).is_ok());
    // Eight such documents in one stream: each is within its budget.
    let stream = [doc.as_str(); 8].join("---\n");
    let docs: Vec<Value> = Deserializer::from_str(&stream)
        .map(Value::deserialize)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(docs.len(), 8);
    // Unread documents are skipped through the same budget, per document.
    let mut it = Deserializer::from_str(&stream);
    for _ in 0..7 {
        let _ = it.next().unwrap();
    }
    let last = it.next().unwrap();
    assert!(Value::deserialize(last).is_ok());
    // A single document over its own budget still fails.
    let tight = Settings::new().limits(Limits::default().alias_budget(100_000));
    let docs: Vec<Result<Value, oneyaml::Error>> = Deserializer::from_str_with(&stream, &tight)
        .map(Value::deserialize)
        .collect();
    assert_eq!(
        docs[0].as_ref().unwrap_err().code(),
        code::ALIAS_BUDGET_EXCEEDED
    );
}

fn nested_value(depth: usize) -> Value {
    let mut v = Value::Null;
    for _ in 0..depth {
        v = Value::from(vec![v]);
    }
    v
}

#[test]
fn serialize_refuses_values_deeper_than_max_typed_depth() {
    // Third-party serializers recurse per level, so `Value`
    // refuses more than the default `max_typed_depth` (128) levels.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let ok = nested_value(128);
            let json = serde_json::to_string(&ok).unwrap();
            assert_eq!(json.len(), 260); // 128 × "[]", plus "null"
            let e = serde_json::to_string(&nested_value(129)).unwrap_err();
            let msg = e.to_string();
            assert!(msg.contains("Y2007"), "{msg}");
            assert!(msg.contains("128"), "{msg}");
            assert!(msg.contains("max_typed_depth"), "{msg}");
            // Mapping and Sequence on their own count their own level.
            let Value::Sequence(seq) = nested_value(128) else {
                return;
            };
            assert!(serde_json::to_string(&seq).is_ok());
            let Value::Sequence(seq) = nested_value(129) else {
                return;
            };
            assert!(serde_json::to_string(&seq).is_err());
            let mut m = oneyaml::Mapping::new();
            m.insert(Value::from("k"), nested_value(127));
            assert!(serde_json::to_string(&m).is_ok());
            let mut m = oneyaml::Mapping::new();
            m.insert(Value::from("k"), nested_value(128));
            assert!(serde_json::to_string(&m).is_err());
        })
        .unwrap()
        .join()
        .unwrap();
}

fn tagged(v: Value) -> Value {
    Value::Tagged(Box::new(oneyaml::TaggedValue {
        tag: oneyaml::Tag::new("!t"),
        value: v,
    }))
}

#[test]
fn serialize_counts_tags_as_levels() {
    // A tag serializes as a one-entry map, one native frame per
    // tag, so tags count toward the serialize depth like collections.
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            // A chain of 200,000 tags: refused (Y2007), never a stack
            // overflow.
            let mut v = Value::Null;
            for _ in 0..200_000 {
                v = tagged(v);
            }
            let e = serde_json::to_string(&v).unwrap_err();
            assert!(e.to_string().contains("Y2007"), "{e}");
            // 128 sequences, each under a tag: 256 serde levels, refused.
            let mut v = Value::Null;
            for _ in 0..128 {
                v = tagged(Value::from(vec![v]));
            }
            let e = serde_json::to_string(&v).unwrap_err();
            assert!(e.to_string().contains("Y2007"), "{e}");
            // The sequence under the outermost tag: 255 levels.
            let seq = v.as_tagged().unwrap().value.as_sequence().unwrap().clone();
            assert!(serde_json::to_string(&seq).is_err());
            let mut m = oneyaml::Mapping::new();
            m.insert(Value::from("k"), Value::from(seq));
            assert!(serde_json::to_string(&m).is_err());
            // 64 tagged sequences: 128 levels, the limit, accepted.
            let mut v = Value::Null;
            for _ in 0..64 {
                v = tagged(Value::from(vec![v]));
            }
            assert!(serde_json::to_string(&v).is_ok());
            // 128 tags alone are 128 levels; one more is refused.
            let mut v = Value::Null;
            for _ in 0..128 {
                v = tagged(v);
            }
            assert!(serde_json::to_string(&v).is_ok());
            assert!(serde_json::to_string(&tagged(v)).is_err());
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A reader that counts the bytes it hands out.
struct Counting<'a> {
    data: &'a [u8],
    read: usize,
}

impl std::io::Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.data.len().min(buf.len()).min(3);
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        self.read = self.read.saturating_add(n);
        Ok(n)
    }
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn utf32be(s: &str) -> Vec<u8> {
    s.chars().flat_map(|c| u32::from(c).to_be_bytes()).collect()
}

#[test]
fn reader_cap_follows_the_encoding() {
    let limit = 16;
    let s = Settings::new().limits(Limits::default().max_document_size(limit));
    let text = "[1, 2, 3, 4, 5]\n"; // exactly 16 bytes
    assert_eq!(text.len(), limit);
    for bytes in [
        text.as_bytes().to_vec(),
        utf16le(text),
        utf32be(text),
        utf16le("\u{feff}[1, 2, 3, 4]\n"),
    ] {
        let v: Value = from_reader_with(&bytes[..], &s).unwrap();
        assert!(v.is_sequence());
    }
    // Far larger input: rejected after reading at most the encoding's cap
    // plus one byte, not 4 × the limit.
    let big = "x".repeat(10_000);
    for (bytes, cap) in [
        (big.as_bytes().to_vec(), 19usize),
        (utf16le(&big), 34),
        (utf32be(&big), 68),
    ] {
        let mut r = Counting {
            data: &bytes,
            read: 0,
        };
        let e = from_reader_with::<_, Value>(&mut r, &s).unwrap_err();
        assert_eq!(e.code(), code::DOCUMENT_TOO_LARGE);
        assert!(
            r.read <= cap.saturating_add(1),
            "read {} > cap {cap} + 1",
            r.read
        );
    }
    // Within the byte cap but over the limit once decoded: the parser's
    // own check.
    let e = from_reader_with::<_, Value>("[1, 2, 3, 4, 5, 6]".as_bytes(), &s).unwrap_err();
    assert_eq!(e.code(), code::DOCUMENT_TOO_LARGE);
    // Shorter than four bytes.
    assert_eq!(from_reader_with::<_, u8>(&b"7"[..], &s).unwrap(), 7);
    let v: Value = from_str_with("", &s).unwrap();
    assert!(v.is_null());
}
