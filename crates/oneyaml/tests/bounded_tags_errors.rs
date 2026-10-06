//! Bounded tag resolution (time and
//! memory), bounded error messages and paths, `<<`/`=` never written plain,
//! limit codes from edits, and rendering many diagnostics with one index.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use oneyaml::{
    Compat, Document, EditError, Limits, Mapping, Render, Settings, Value, code, from_str,
    from_str_with, path, to_string, to_string_with,
};

fn untrusted() -> Settings {
    Settings::new().limits(Limits::untrusted())
}

/// `settings` without the alias byte budget, for the
/// checks of what a read costs once it is allowed to expand that much.
fn no_byte_budget(settings: Settings) -> Settings {
    let limits = settings.get_limits().clone().alias_byte_budget(usize::MAX);
    settings.limits(limits)
}

/// The timed tests run one at a time (the harness runs tests in parallel).
static TIMING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The minimum of 3 runs of `f`.
fn best(f: impl Fn()) -> Duration {
    let mut min = Duration::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        f();
        min = min.min(t.elapsed());
    }
    min
}

fn ratio(small: Duration, large: Duration) -> f64 {
    large.as_secs_f64() / small.as_secs_f64().max(1e-9)
}

// ======================================================== 1-2: tag bounds

/// A long `%TAG` prefix used by `tags` nodes (`distinct`: each its own
/// suffix).
fn tagged(prefix: usize, tags: usize, distinct: bool) -> String {
    let mut s = format!("%TAG !e! tag:x,1:{}\n---\n", "a".repeat(prefix));
    for i in 0..tags {
        if distinct {
            s.push_str(&format!("- !e!x{i} v\n"));
        } else {
            s.push_str("- !e!x v\n");
        }
    }
    s
}

/// The known triggers, scaled: 10× the tags of one 256 KB prefix
/// cost about the same (the input grows 1.1×), for the events, the
/// `Document` (its meaning check), and the `Value`; O(tags × prefix)
/// would be about 10×.
///
/// The `Value`'s logical size (768 MB of tag text for 3,000
/// uses) is now over the default alias byte budget, so the default read
/// refuses it (`Y2009`, the expected new refusal); the timing below is of
/// a read with the byte budget lifted.
#[test]
fn repeated_tags_of_a_long_prefix_scale_with_the_input() {
    let _serial = serial();
    let (small, large) = (
        tagged(256 * 1024, 300, false),
        tagged(256 * 1024, 3000, false),
    );
    assert_eq!(
        from_str::<Value>(&large).unwrap_err().code(),
        code::ALIAS_BYTE_BUDGET_EXCEEDED
    );
    let lifted = no_byte_budget(Settings::new());
    for (what, run) in [
        (
            "events",
            Box::new(|s: &str| {
                let n = oneyaml_syntax::parse(s).iter().count();
                assert!(n > 0);
            }) as Box<dyn Fn(&str)>,
        ),
        (
            "document",
            Box::new(|s: &str| {
                Document::parse(s).unwrap();
            }),
        ),
        (
            "value",
            Box::new(|s: &str| {
                from_str_with::<Value>(s, &lifted).unwrap();
            }),
        ),
    ] {
        let (a, b) = (best(|| run(&small)), best(|| run(&large)));
        println!("{what}: 300 tags {a:?}, 3000 tags {b:?}");
        assert!(ratio(a, b) < 3.0, "{what}: {:.2}x", ratio(a, b));
    }
}

/// Memory: the values of one read share each distinct tag's text.
#[test]
fn values_share_repeated_tags() {
    let v: Value = from_str(&tagged(4096, 50, false)).unwrap();
    let items = v.as_sequence().unwrap();
    let first = &items.get(0).unwrap().as_tagged().unwrap().tag;
    assert_eq!(first.len(), "tag:x,1:".len() + 4096 + 1);
    for item in items {
        let t = &item.as_tagged().unwrap().tag;
        assert!(t.same_allocation(first), "not shared");
    }
    // Distinct tags stay distinct, and equal to their resolution.
    let v: Value = from_str(&tagged(16, 3, true)).unwrap();
    let tags: Vec<String> = v
        .as_sequence()
        .unwrap()
        .iter()
        .map(|i| i.as_tagged().unwrap().tag.to_string())
        .collect();
    let p = "a".repeat(16);
    assert_eq!(
        tags,
        [
            format!("tag:x,1:{p}x0"),
            format!("tag:x,1:{p}x1"),
            format!("tag:x,1:{p}x2")
        ]
    );
    // The Document reads the same values.
    let src = tagged(4096, 5, true);
    assert_eq!(
        Document::parse(&src).unwrap().to_value().unwrap(),
        from_str::<Value>(&src).unwrap()
    );
}

/// Limits: `untrusted()` refuses an 8 MB prefix at its directive, and a
/// resolved tag past `max_scalar_len`, with `Y2001`.
#[test]
fn long_prefixes_and_tags_are_limit_errors() {
    let big = tagged(8 << 20, 2, false);
    let e = from_str_with::<Value>(&big, &untrusted()).unwrap_err();
    assert_eq!(e.code(), code::SCALAR_TOO_LONG);
    assert_eq!(e.location().unwrap().line(), 1);
    assert_eq!(
        Document::parse_with(&big, &untrusted()).unwrap_err().code(),
        code::SCALAR_TOO_LONG
    );
    // Prefix just under 1 MiB, suffix pushing the tag over it.
    let prefix = (1 << 20) - "tag:x,1:".len();
    let src = format!("%TAG !e! tag:x,1:{}\n--- !e!xy v\n", "a".repeat(prefix - 1));
    let e = from_str_with::<Value>(&src, &untrusted()).unwrap_err();
    assert_eq!(e.code(), code::SCALAR_TOO_LONG);
    assert_eq!(e.location().unwrap().line(), 2);
    // Under the defaults both read.
    assert!(from_str::<Value>(&big).is_ok());
    assert!(from_str::<Value>(&src).is_ok());
}

// ==================================================== 3: bounded errors

/// A long key anchored once and used through aliases at every level of a
/// `depth`-deep mapping, with a duplicate at the bottom: every path
/// segment and the message name the key.
fn deep_alias_keys(key_len: usize, depth: usize) -> String {
    let mut s = format!("k: &k \"{}\"\nm:\n", "x".repeat(key_len));
    for d in 0..depth {
        s.push_str(&" ".repeat(2 * (d + 1)));
        s.push_str("*k :\n");
    }
    let pad = " ".repeat(2 * (depth + 1));
    s.push_str(&format!("{pad}*k : 1\n{pad}*k : 2\n"));
    s
}

fn check_bounded(e: &oneyaml::Error, src: &str) {
    let msg = &e.diagnostic().message;
    assert!(msg.len() <= 3072, "message {} bytes", msg.len());
    let shown = e.to_string();
    assert!(
        shown.len() <= 3072 + 8192 + 64,
        "display {} bytes",
        shown.len()
    );
    for seg in e.path().unwrap().segments() {
        if let oneyaml::PathSegment::Key(k) = seg {
            assert!(k.chars().count() <= 65, "segment of {} chars", k.len());
        }
    }
    let rendered = e.render(src);
    assert!(
        rendered.len() <= 16 * 1024,
        "render {} bytes",
        rendered.len()
    );
    // The span stays exact.
    let span = e.span().unwrap();
    assert!(src.get(span.start..span.end).unwrap().starts_with("*k"));
}

/// The known trigger: a 1 MB input under `untrusted()` gave a 60 MB
/// message. Now each segment is at most 64 characters (then `…`), the
/// message at most 3 KiB and what `Display` shows of the path 8 KiB.
///
/// The 31 MB of alias-expanded keys are over `untrusted()`'s
/// alias byte budget (8 MiB), so that read now stops there (`Y2009`, the
/// expected new refusal), with a bounded error too; the duplicate-key
/// error is checked under `untrusted()` with the byte budget lifted.
#[test]
fn alias_expanded_keys_give_bounded_errors() {
    let src = deep_alias_keys(1 << 19, 60);
    let e = from_str_with::<Value>(&src, &untrusted()).unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    check_bounded(&e, &src);
    for s in [Settings::new(), no_byte_budget(untrusted())] {
        let e = from_str_with::<Value>(&src, &s).unwrap_err();
        assert_eq!(e.code(), code::DUPLICATE_KEY);
        check_bounded(&e, &src);
        assert_eq!(e.path().unwrap().segments().len(), 61);
        let e = Document::parse_with(src.as_str(), &s).unwrap_err();
        check_bounded(&e, &src);
    }
}

/// A path longer than what `Display` shows (8 KiB) is cut there with
/// `…`; `Error::path` keeps every segment.
#[test]
fn long_paths_are_cut_where_shown_only() {
    let src = deep_alias_keys(100, 200);
    let e = from_str::<Value>(&src).unwrap_err();
    check_bounded(&e, &src);
    assert_eq!(e.path().unwrap().segments().len(), 201);
    let shown = e.to_string();
    assert!(shown.contains("… ("), "{}", &shown[shown.len() - 40..]);
}

/// 1× and 10× key length (and depth): the error's size does not grow.
///
/// The large input's 100 MB of alias-expanded keys are over the
/// default alias byte budget (`Y2009`, the expected new refusal); the
/// checks below read with the byte budget lifted.
#[test]
fn error_size_does_not_scale_with_keys() {
    let _serial = serial();
    let (small, large) = (deep_alias_keys(1 << 16, 10), deep_alias_keys(1 << 20, 100));
    let e = from_str::<Value>(&large).unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    check_bounded(&e, &large);
    let lifted = no_byte_budget(Settings::new());
    let from_str = |s: &str| from_str_with::<Value>(s, &lifted);
    let e1 = from_str(&small).unwrap_err();
    let e2 = from_str(&large).unwrap_err();
    check_bounded(&e1, &small);
    check_bounded(&e2, &large);
    assert_eq!(e1.diagnostic().message, e2.diagnostic().message);
    assert!(e2.to_string().len() <= 3072 + 8192 + 64);
    // Building the error costs little beside reading the input (whose
    // alias-expanded keys cost O(key) per use, within the alias
    // byte budget): the same document without the duplicate.
    let valid = large.replace("*k : 2\n", "");
    let read = best(|| drop(from_str(&valid).unwrap()));
    let fail = best(|| drop(from_str(&large).unwrap_err()));
    println!("read {read:?}, error {fail:?}");
    assert!(ratio(read, fail) < 3.0, "{:.2}x", ratio(read, fail));
}

/// Messages quoting input (a long string into a number, an unknown field)
/// are built within 3 KiB; long keys in paths are cut to 64 characters.
#[test]
fn messages_and_segments_are_cut_with_a_marker() {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Port {
        port: u16,
    }
    let long = "y".repeat(1 << 20);
    let src = format!("port: \"{long}\"\n");
    let e = from_str::<Port>(&src).unwrap_err();
    assert!(e.diagnostic().message.len() <= 3072);
    assert!(e.diagnostic().message.ends_with("… [message truncated]"));
    let src = format!("? {long}\n: 1\n");
    let e = from_str::<BTreeMap<String, bool>>(&src).unwrap_err();
    let key = e.path().unwrap().to_string();
    assert_eq!(key, format!("{}…", "y".repeat(64)));
    // Short ones are unchanged.
    let e = from_str::<BTreeMap<String, bool>>("abc: 1\n").unwrap_err();
    assert_eq!(e.path().unwrap().to_string(), "abc");
    // A caller's own long diagnostic renders within the bound too.
    let d = oneyaml::Diagnostic::error(
        code::CUSTOM,
        "z".repeat(1 << 20),
        oneyaml::Label::new(oneyaml::Span::new(0, 1), ""),
    );
    assert!(d.render("a").len() < 8 * 1024);
}

// ======================================================= 4: `<<` and `=`

/// An injection: a string key `<<` written plain would merge its value
/// into the parent in every merge-key reader. Every layout quotes it (and
/// `=`), in serializer and edit output.
#[test]
fn merge_key_strings_are_never_written_plain() {
    let mut inner = Mapping::new();
    inner.insert(Value::from("admin"), Value::from(true));
    let mut m = Mapping::new();
    m.insert(Value::from("<<"), Value::from(inner));
    m.insert(Value::from("="), Value::from("x"));
    m.insert(Value::from("user"), Value::from("bob"));
    let v = Value::from(m);
    let merging = Settings::new().merge_keys(true);
    for s in [Settings::new(), Compat::serde_yaml(), Compat::yaml_1_1()] {
        let out = to_string_with(&v, &s).unwrap();
        assert!(!out.contains("\n<<") && !out.starts_with("<<"), "{out}");
        assert!(!out.contains("\n=:") && !out.starts_with("=:"), "{out}");
        // Read back with merge keys on: still the string key, no merge.
        let back: Value = from_str_with(&out, &merging).unwrap();
        assert_eq!(back, v, "{out}");
        assert!(back.get("admin").is_none());
    }
    assert_eq!(
        to_string_with(&vec!["<<", "="], &Compat::serde_yaml()).unwrap(),
        "- '<<'\n- '='\n"
    );
    assert_eq!(to_string(&vec!["<<", "="]).unwrap(), "- '<<'\n- '='\n");
    // The edit layer writes the same.
    let mut doc = Document::parse_with("user: bob\n", &merging).unwrap();
    doc.root_mut().unwrap().insert("<<", "x").unwrap();
    doc.root_mut().unwrap().insert("=", "z").unwrap();
    assert_eq!(doc.to_string(), "user: bob\n'<<': x\n'=': z\n");
    assert_eq!(doc.to_value().unwrap()["<<"].as_str(), Some("x"));
}

// ============================================================ 5: edits

/// A new scalar past `max_scalar_len` is refused with `Y2001`, not as text
/// that would not read back; nothing changes.
#[test]
fn edits_past_a_limit_report_its_code() {
    let mut doc = Document::parse_with("a: 1\n", &untrusted()).unwrap();
    let big = "q".repeat(2 << 20);
    let id = doc.id_of(&path!["a"]).unwrap();
    let e = doc.node_mut(id).unwrap().set_scalar(&big).unwrap_err();
    assert!(matches!(e, EditError::Rejected { .. }), "{e:?}");
    assert_eq!(e.diagnostic().unwrap().code, code::SCALAR_TOO_LONG);
    assert_eq!(doc.to_string(), "a: 1\n");
    let e = doc.set_path(&path!["b"], &big).unwrap_err();
    assert_eq!(e.diagnostic().unwrap().code, code::SCALAR_TOO_LONG);
    assert_eq!(doc.to_string(), "a: 1\n");
}

// ========================================================== 6: rendering

#[test]
fn all_diagnostics_render_with_one_index() {
    let src = "%YAML 1.1\n--- a: b: c\n";
    let parsed = Document::parse_recovering(src);
    let d = parsed.diagnostics();
    assert!(d.len() >= 2);
    let each: Vec<String> = d.iter().map(|x| x.render(src)).collect();
    assert_eq!(parsed.render_diagnostics(src), each.join("\n"));
    assert_eq!(parsed.value().render_diagnostics(), each.join("\n"));
    assert_eq!(Document::parse("a: 1\n").unwrap().render_diagnostics(), "");
}
