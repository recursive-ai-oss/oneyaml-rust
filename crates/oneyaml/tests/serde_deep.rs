//! Deep input on small stacks never crashes the serde path.
//! `Value`, ignored content, and alias replay are iterative at any depth;
//! typed recursion into recursive user types is bounded by
//! `max_typed_depth`. Run in release too (`cargo test --release`):
//! the stack cost per level differs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Limits, Settings, Value, code, from_str, from_str_with};
use serde::Deserialize;

const SMALL: usize = 256 * 1024;

fn on_small_stack<T: Send + 'static>(stack: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn nested(depth: usize) -> String {
    format!("{}{}", "[".repeat(depth), "]".repeat(depth))
}

#[test]
fn value_at_a_million_levels_is_iterative() {
    on_small_stack(SMALL, || {
        let unlimited = Settings::new().limits(Limits::unlimited());
        let src = nested(1_000_000);
        let v: Value = from_str_with(&src, &unlimited).unwrap();
        assert_eq!(v.depth(), 1_000_000);
        // Dropped here, iteratively.
    });
}

#[test]
fn default_limits_reject_deep_input_on_every_target() {
    on_small_stack(SMALL, || {
        let src = nested(1_000_000);
        assert_eq!(
            from_str::<Value>(&src).unwrap_err().code(),
            code::DEPTH_LIMIT_EXCEEDED
        );
        // A non-recursive target never follows the input's depth.
        #[derive(Debug, Deserialize)]
        struct Flat {
            #[allow(dead_code)]
            a: Option<u8>,
        }
        let src = format!("b: {}\n", nested(1_000_000));
        assert_eq!(
            from_str::<Flat>(&src).unwrap_err().code(),
            code::DEPTH_LIMIT_EXCEEDED
        );
    });
}

#[test]
fn ignored_and_value_fields_are_iterative() {
    on_small_stack(SMALL, || {
        let unlimited = Settings::new().limits(Limits::unlimited());
        let src = format!(
            "skip: {}\nkeep: {}\nn: 1\n",
            nested(200_000),
            nested(200_000)
        );
        #[derive(Deserialize)]
        struct S {
            keep: Value,
            n: u8,
        }
        let s: S = from_str_with(&src, &unlimited).unwrap();
        assert_eq!((s.keep.depth(), s.n), (200_000, 1));
    });
}

#[test]
fn deep_alias_replay_is_iterative() {
    on_small_stack(SMALL, || {
        let unlimited = Settings::new().limits(Limits::unlimited());
        let src = format!("- &a {}\n- *a\n- *a\n", nested(100_000));
        let v: Value = from_str_with(&src, &unlimited).unwrap();
        assert_eq!(v[2].depth(), 100_000);
        // Under default limits, expansion that nests too deep is rejected.
        let src = format!(
            "- &a {}\n- {}*a{}\n",
            nested(1000),
            "[".repeat(100),
            "]".repeat(100)
        );
        assert_eq!(
            from_str::<Value>(&src).unwrap_err().code(),
            code::DEPTH_LIMIT_EXCEEDED
        );
    });
}

/// A recursive user type: serde recursion follows the input here.
#[derive(Debug, Deserialize)]
struct Tree(Vec<Tree>);

fn tree_depth(t: &Tree) -> usize {
    let mut depth = 1usize;
    let mut cur = t;
    while let Some(next) = cur.0.first() {
        depth = depth.saturating_add(1);
        cur = next;
    }
    depth
}

/// The stack of a typical spawned thread (Rust's default).
const THREAD_STACK: usize = 2 * 1024 * 1024;

#[test]
fn typed_recursion_is_bounded_by_max_typed_depth() {
    // `max_typed_depth` (128 by default) bounds typed recursion, so
    // hostile deep input at default limits is a limit error, never a stack
    // overflow, on a 2 MiB thread in debug and release builds alike.
    on_small_stack(THREAD_STACK, || {
        for depth in [129, 1000, 1024, 1025, 100_000, 1_000_000] {
            let e = from_str::<Tree>(&nested(depth)).unwrap_err();
            assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED, "{depth}");
        }
        let t: Tree = from_str(&nested(128)).unwrap();
        assert_eq!(tree_depth(&t), 128);
        // The error names the setting and its value, and points at the
        // first collection too deep.
        let e = from_str::<Tree>(&nested(129)).unwrap_err();
        let help = e.diagnostic().help.clone().unwrap();
        assert!(
            help.contains("Limits::max_typed_depth (currently 128)"),
            "{help}"
        );
        assert_eq!(e.span().map(|s| s.start), Some(128));
        // Block syntax and mappings count the same way.
        #[derive(Debug, Deserialize)]
        struct Node {
            #[allow(dead_code)]
            next: Option<Box<Node>>,
        }
        let mut src = String::new();
        for i in 0..200 {
            src.push_str(&"  ".repeat(i));
            src.push_str("next:\n");
        }
        assert_eq!(
            from_str::<Node>(&src).unwrap_err().code(),
            code::TYPED_DEPTH_EXCEEDED
        );
    });
}

#[test]
fn max_typed_depth_profiles_and_raising() {
    on_small_stack(THREAD_STACK, || {
        let untrusted = Settings::new().limits(Limits::untrusted());
        assert!(from_str_with::<Tree>(&nested(64), &untrusted).is_ok());
        assert_eq!(
            from_str_with::<Tree>(&nested(65), &untrusted)
                .unwrap_err()
                .code(),
            code::TYPED_DEPTH_EXCEEDED
        );
        let low = Settings::new().limits(Limits::new().max_typed_depth(3));
        assert!(from_str_with::<Tree>("[[[]]]", &low).is_ok());
        assert_eq!(
            from_str_with::<Tree>("[[[[]]]]", &low).unwrap_err().code(),
            code::TYPED_DEPTH_EXCEEDED
        );
        // Only the typed path is bounded: `Value` (and `Value` fields)
        // are iterative and bounded by `max_depth` alone.
        let v: Value = from_str(&nested(1024)).unwrap();
        assert_eq!(v.depth(), 1024);
        #[derive(Deserialize)]
        struct Holder {
            v: Value,
        }
        let h: Holder = from_str_with(&format!("v: {}", nested(1000)), &low).unwrap();
        assert_eq!(h.v.depth(), 1000);
        // Wide is not deep.
        let wide = format!("[{}]", vec!["[[]]"; 10_000].join(","));
        assert!(from_str_with::<Tree>(&wide, &low).is_ok());
    });
    // Raised values need a bigger stack (documented on the setting):
    // 1024 levels (the default `max_depth`) fit 16 MiB in both profiles.
    on_small_stack(16 * 1024 * 1024, || {
        let raised = Settings::new().limits(Limits::new().max_typed_depth(1024));
        let t: Tree = from_str_with(&nested(1024), &raised).unwrap();
        assert_eq!(tree_depth(&t), 1024);
        // Beyond it, `max_depth` still applies.
        assert_eq!(
            from_str_with::<Tree>(&nested(1025), &raised)
                .unwrap_err()
                .code(),
            code::DEPTH_LIMIT_EXCEEDED
        );
    });
}

/// `{<<: {<<: … {a: 1} … }}`, `depth` merges deep.
fn nested_merges(depth: usize) -> String {
    format!("{}{{a: 1}}{}", "{<<: ".repeat(depth), "}".repeat(depth))
}

#[test]
fn nested_merge_sources_are_planned_iteratively() {
    // Merge planning never recurses per level of merge
    // sources (the input sets that depth, not the target type), at
    // default limits on a 2 MiB thread, and on a small stack when the
    // limits are off.
    use std::collections::HashMap;
    on_small_stack(THREAD_STACK, || {
        let on = Settings::new().merge_keys(true);
        let m: HashMap<String, i64> = from_str_with(&nested_merges(500), &on).unwrap();
        assert_eq!(m, HashMap::from([("a".to_owned(), 1)]));
        let v: Value = from_str_with(&nested_merges(500), &on).unwrap();
        assert_eq!(v, from_str::<Value>("{a: 1}").unwrap());
    });
    on_small_stack(SMALL, || {
        let on = Settings::new().merge_keys(true).limits(Limits::unlimited());
        let m: HashMap<String, i64> = from_str_with(&nested_merges(100_000), &on).unwrap();
        assert_eq!(m.len(), 1);
        // Ignored merge content is checked iteratively too.
        #[derive(Deserialize)]
        struct K {
            k: u8,
        }
        let src = format!("x: {}\nk: 1\n", nested_merges(100_000));
        assert_eq!(from_str_with::<K>(&src, &on).unwrap().k, 1);
    });
}
