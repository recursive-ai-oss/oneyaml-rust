//! The real-world examples as edit tests (a Kubernetes
//! manifest, a GitHub Actions workflow, a docker-compose file with merge
//! keys), and the deep-edit test on a small stack (no recursion, no crash).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, Limits, Path, Settings, code, path};

const K8S: &str = "\
# Deployment for the web tier
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    app: web # selector label
spec:
  replicas: 2
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
    spec:
      containers:
        - name: nginx
          image: \"nginx:1.14.2\"
          ports:
            - containerPort: 80
";

#[test]
fn kubernetes_manifest() {
    let mut d = Document::parse(K8S).unwrap();
    d.set_path(&path!["spec", "replicas"], 5).unwrap();
    d.set_path(
        &path!["spec", "template", "spec", "containers", 0, "image"],
        "nginx:1.16.1",
    )
    .unwrap();
    // A new container, laid out like the first.
    let mut containers = d
        .get_path_mut(&path!["spec", "template", "spec", "containers"])
        .unwrap();
    #[derive(serde::Serialize)]
    struct Container {
        name: &'static str,
        image: &'static str,
    }
    containers
        .push(&Container {
            name: "sidecar",
            image: "busybox",
        })
        .unwrap();
    let want = K8S
        .replace("replicas: 2", "replicas: 5")
        .replace("nginx:1.14.2", "nginx:1.16.1")
        + "        - name: sidecar\n          image: busybox\n";
    assert_eq!(d.to_string(), want);
    agrees_with_its_text(&d);
    // Keeping the double quotes: a scalar keeps its style.
    let img = d
        .id_of(&path!["spec", "template", "spec", "containers", 0, "image"])
        .unwrap();
    d.node_mut(img).unwrap().set_scalar("nginx:1.17").unwrap();
    assert!(d.to_string().contains("image: \"nginx:1.17\"\n"));
    agrees_with_its_text(&d);
}

const ACTIONS: &str = "\
name: CI
on:
  push:
    branches: [main]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@v4
    # build it
    - name: Build
      run: cargo build --verbose
";

#[test]
fn github_actions_workflow() {
    let mut d = Document::parse(ACTIONS).unwrap();
    let mut steps = d.get_path_mut(&path!["jobs", "test", "steps"]).unwrap();
    #[derive(serde::Serialize)]
    struct Step {
        name: &'static str,
        run: &'static str,
    }
    steps
        .push(&Step {
            name: "Test",
            run: "cargo test --verbose",
        })
        .unwrap();
    d.get_path_mut(&path!["on", "push", "branches"])
        .unwrap()
        .push("release/*")
        .unwrap();
    let want = ACTIONS.replace("[main]", "[main, release/*]")
        + "    - name: Test\n      run: cargo test --verbose\n";
    assert_eq!(d.to_string(), want);
    agrees_with_its_text(&d);
    // Insert a step before the build: `# build it` leads the build step
    // (comment rule 4) and stays with it, so the new step goes above it
    // (before, it went between the comment and the build step,
    // which gave the comment to the new step).
    let mut steps = d.get_path_mut(&path!["jobs", "test", "steps"]).unwrap();
    steps
        .insert_at(
            1,
            serde_json::json!({"uses": "dtolnay/rust-toolchain@stable"}),
        )
        .unwrap();
    assert!(d.to_string().contains(
        "    - uses: actions/checkout@v4\n    - uses: dtolnay/rust-toolchain@stable\n    # build it\n    - name: Build\n"
    ));
    agrees_with_its_text(&d);
}

const COMPOSE: &str = "\
x-defaults: &defaults
  restart: always
  image: app:1.0
services:
  web:
    <<: *defaults
    ports:
      - \"80:80\"
  worker:
    <<: *defaults
    command: work
";

#[test]
fn docker_compose_with_merge_keys() {
    let s = Settings::new().merge_keys(true);
    let mut d = Document::parse_with(COMPOSE, &s).unwrap();
    // Edit the merged key's target: every service sees it (aliases denote the
    // node).
    let image = d.id_of(&path!["services", "web", "image"]).unwrap();
    d.node_mut(image).unwrap().set("app:2.0").unwrap();
    assert_eq!(
        d.to_value().unwrap()["services"]["worker"]["image"].as_str(),
        Some("app:2.0")
    );
    // Insert a shadowing key in one service (merge keys get no special
    // treatment).
    d.set_path(&path!["services", "worker", "image"], "worker:1")
        .unwrap();
    assert_eq!(
        d.to_value().unwrap()["services"]["worker"]["image"].as_str(),
        Some("worker:1")
    );
    assert_eq!(
        d.to_value().unwrap()["services"]["web"]["image"].as_str(),
        Some("app:2.0")
    );
    let want = COMPOSE.replace("app:1.0", "app:2.0") + "    image: worker:1\n";
    assert_eq!(d.to_string(), want);
    agrees_with_its_text(&d);
    // The anchor cannot go while services merge it.
    let defaults = d.id_of(&path!["x-defaults"]).unwrap();
    assert_eq!(d.aliases_of(defaults).unwrap().len(), 2);
    assert!(d.remove_path(&path!["x-defaults"]).is_err());
}

fn small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// Edits at depth 1,000,000 on a small stack end in `Ok` or
/// `Err`, never a crash.
#[test]
fn set_path_a_million_levels_deep() {
    small_stack(|| {
        const N: usize = 1_000_000;
        let mut p = Path::new();
        for _ in 0..N {
            p.push_index(0);
        }
        // An empty document: the nested value is past `max_depth` (Y2005).
        let mut d = Document::parse("").unwrap();
        let e = d.set_path(&p, 1).unwrap_err();
        assert_eq!(e.diagnostic().unwrap().code, code::VALUE_DEPTH_EXCEEDED);
        assert_eq!(d.to_string(), "");
        // A document that deep (no limits): the innermost scalar.
        let s = Settings::new().limits(Limits::unlimited());
        let src = format!("{}1{}", "[".repeat(N), "]".repeat(N));
        let mut d = Document::parse_with(src, &s).unwrap();
        match d.set_path(&p, 2) {
            Ok(_) => {
                let text = d.to_string();
                assert_eq!(text.len(), 2 * N + 1);
                assert_eq!(&text[N..=N], "2");
            }
            Err(e) => eprintln!("refused at depth {N}: {e}"),
        }
        drop(d);
    });
}
