//! Measures the minimal thread stack for *typed* serde recursion at the
//! default `max_depth`, with `max_typed_depth` lifted (evidence for the
//! per-level costs documented on `Limits::max_typed_depth`).
//! Bisects by running itself as a child process; children that overflow
//! abort by design (and may leave core dumps where core dumps are on).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
use serde::Deserialize;
#[derive(Deserialize)]
struct Tree(#[allow(dead_code)] Vec<Tree>);
#[derive(Deserialize)]
struct Node {
    #[allow(dead_code)]
    #[serde(default)]
    name: String,
    #[allow(dead_code)]
    #[serde(default)]
    children: Vec<Node>,
}
fn ok(stack: usize, src: String, which: u8) -> bool {
    let h = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("child")
        .arg(stack.to_string())
        .arg(which.to_string())
        .arg(src.len().to_string())
        .output()
        .unwrap();
    h.status.success()
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.get(1).map(String::as_str) == Some("child") {
        let stack: usize = a[2].parse().unwrap();
        let which: u8 = a[3].parse().unwrap();
        let depth: usize = a[4].parse().unwrap();
        let src = if which == 0 {
            format!("{}{}", "[".repeat(depth), "]".repeat(depth))
        } else {
            format!("{}{}", "{children: [".repeat(depth), "]}".repeat(depth))
        };
        std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                // Lift `max_typed_depth` (default 128) so the probe measures
                // the real per-level cost at the full depth.
                let s = oneyaml::Settings::new()
                    .limits(oneyaml::Limits::new().max_typed_depth(usize::MAX));
                let _ = if which == 0 {
                    oneyaml::from_str_with::<Tree>(&src, &s).map(|_| ())
                } else {
                    oneyaml::from_str_with::<Node>(&src, &s).map(|_| ())
                };
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for which in [0u8, 1] {
        let depth = if which == 0 { 1024 } else { 512 };
        let (mut lo, mut hi) = (16 * 1024usize, 64 * 1024 * 1024usize);
        while hi - lo > 16 * 1024 {
            let mid = (lo + hi) / 2;
            if ok(mid, "x".repeat(depth), which) {
                hi = mid
            } else {
                lo = mid
            }
        }
        println!(
            "type {which} depth {depth}: min stack ~{} KiB ({} B/level)",
            hi / 1024,
            hi / depth
        );
    }
}
