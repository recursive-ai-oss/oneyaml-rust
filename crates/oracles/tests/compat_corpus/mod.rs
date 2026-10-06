//! The hand-built migration corpus of the compat differential
//! (`tests/compat_differential.rs`): what real `serde_yaml` users write —
//! Kubernetes manifests, GitHub Actions workflows, docker-compose files with
//! merge keys, application configs with YAML 1.1-looking scalars (`yes`,
//! `0644`, `12:30`), duplicate keys, tags, anchors, multi-document streams —
//! each read into the typed models serde users derive for them.
//!
//! Every input is written from scratch for this corpus (no third-party
//! text).

use std::collections::BTreeMap;
use std::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// One observation: what each library did with one input and one target.
/// `Ok` holds a canonical rendering of the value read (or the bytes
/// written); `Err` the library's message (only `Ok`/`Err` is compared).
#[derive(Clone, Debug)]
pub struct Obs {
    pub target: String,
    pub ours: Result<String, String>,
    pub theirs: Result<String, String>,
}

impl Obs {
    /// Both read (or wrote) the same thing, or both refused.
    pub fn agrees(&self) -> bool {
        match (&self.ours, &self.theirs) {
            (Ok(a), Ok(b)) => a == b,
            (Err(_), Err(_)) => true,
            _ => false,
        }
    }
}

/// Reads `src` into `T` with `Compat::serde_yaml()` and with
/// `serde_yaml_ng`, compares the typed values (`Debug`), then writes the
/// value read (theirs, else ours) with `to_string_with(Compat::serde_yaml())`
/// and `serde_yaml_ng::to_string` and compares the bytes.
pub fn typed<T: DeserializeOwned + Serialize + Debug>(name: &str, src: &str) -> Vec<Obs> {
    let settings = oneyaml::Compat::serde_yaml();
    let ours = oneyaml::from_str_with::<T>(src, &settings);
    let theirs = serde_yaml_ng::from_str::<T>(src);
    let mut out = vec![Obs {
        target: format!("t:{name}"),
        ours: ours
            .as_ref()
            .map(|v| format!("{v:?}"))
            .map_err(|e| e.to_string()),
        theirs: theirs
            .as_ref()
            .map(|v| format!("{v:?}"))
            .map_err(|e| e.to_string()),
    }];
    if let Some(v) = theirs.as_ref().ok().or(ours.as_ref().ok()) {
        let w = write_obs(&format!("w:{name}"), v);
        if let Ok(text) = &w.ours {
            // What we write reads back to the same typed value.
            let back: T = oneyaml::from_str_with(text, &settings)
                .unwrap_or_else(|e| panic!("{name}: our output does not read back: {e}"));
            assert_eq!(format!("{back:?}"), format!("{v:?}"), "{name}: read back");
        }
        out.push(w);
    }
    out
}

/// As [`typed`], with merge keys: ours with `Compat::serde_yaml()
/// .merge_keys(true)`, theirs through `Value::apply_merge` then
/// `from_value` (serde_yaml's only merge mechanism).
pub fn typed_merged<T: DeserializeOwned + Debug>(name: &str, src: &str) -> Vec<Obs> {
    let settings = oneyaml::Compat::serde_yaml().merge_keys(true);
    let ours = oneyaml::from_str_with::<T>(src, &settings);
    let theirs = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(src).and_then(|mut v| {
        v.apply_merge()?;
        serde_yaml_ng::from_value::<T>(v)
    });
    vec![Obs {
        target: format!("M:{name}"),
        ours: ours.map(|v| format!("{v:?}")).map_err(|e| e.to_string()),
        theirs: theirs.map(|v| format!("{v:?}")).map_err(|e| e.to_string()),
    }]
}

/// Writes `v` with both libraries.
pub fn write_obs<T: Serialize + ?Sized>(target: &str, v: &T) -> Obs {
    Obs {
        target: target.to_owned(),
        ours: oneyaml::to_string_with(v, &oneyaml::Compat::serde_yaml()).map_err(|e| e.to_string()),
        theirs: serde_yaml_ng::to_string(v).map_err(|e| e.to_string()),
    }
}

/// One migration-corpus input and the typed targets it is read into.
pub struct Input {
    pub name: &'static str,
    pub src: &'static str,
    pub typed: fn(&str) -> Vec<Obs>,
}

fn none(_: &str) -> Vec<Obs> {
    Vec::new()
}

// ---------------------------------------------------------------- models

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    pub on: serde_json::Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub jobs: BTreeMap<String, Job>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Job {
    #[serde(rename = "runs-on")]
    pub runs_on: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Strategy>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Strategy {
    pub matrix: BTreeMap<String, Vec<String>>,
    #[serde(rename = "fail-fast", default)]
    pub fail_fast: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Compose {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub services: BTreeMap<String, Service>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Service {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub debug: bool,
    #[serde(default)]
    pub ratio: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<Database>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Database {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

/// Strings everywhere: a common way to read "whatever the user wrote".
pub type Strings = BTreeMap<String, String>;

/// Enum representations serde_yaml users rely on.
#[derive(Debug, Serialize, Deserialize)]
pub enum Shape {
    Unit,
    Circle { r: f64 },
    Square(f64),
    Pair(i32, i32),
    Named(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Drawing {
    pub shapes: Vec<Shape>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Internal {
    Http { url: String },
    File { path: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Untagged {
    Num(i64),
    Text(String),
    List(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Sources {
    pub sources: Vec<Internal>,
    pub values: Vec<Untagged>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WithFlatten {
    pub name: String,
    #[serde(flatten)]
    pub rest: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Numbers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<i128>,
}

fn manifest(src: &str) -> Vec<Obs> {
    typed::<Manifest>("Manifest", src)
}

fn workflow(src: &str) -> Vec<Obs> {
    typed::<Workflow>("Workflow", src)
}

fn compose(src: &str) -> Vec<Obs> {
    let mut v = typed::<Compose>("Compose", src);
    v.extend(typed_merged::<Compose>("Compose", src));
    v
}

fn app(src: &str) -> Vec<Obs> {
    let mut v = typed::<AppConfig>("AppConfig", src);
    v.extend(typed::<Strings>("Strings", src));
    v
}

fn strings(src: &str) -> Vec<Obs> {
    typed::<Strings>("Strings", src)
}

fn drawing(src: &str) -> Vec<Obs> {
    typed::<Drawing>("Drawing", src)
}

fn sources(src: &str) -> Vec<Obs> {
    typed::<Sources>("Sources", src)
}

fn flatten(src: &str) -> Vec<Obs> {
    typed::<WithFlatten>("WithFlatten", src)
}

fn numbers(src: &str) -> Vec<Obs> {
    typed::<Numbers>("Numbers", src)
}

fn int_map(src: &str) -> Vec<Obs> {
    let mut v = typed::<BTreeMap<u32, String>>("IntKeys", src);
    v.extend(typed::<Strings>("Strings", src));
    v
}

// ---------------------------------------------------------------- inputs

/// The migration corpus.
pub const INPUTS: &[Input] = &[
    Input {
        name: "k8s-deployment",
        src: "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: prod
  labels:
    app: web
    tier: frontend
  annotations:
    deployment.kubernetes.io/revision: \"3\"
spec:
  replicas: 3
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
    spec:
      containers:
        - name: web
          image: nginx:1.27
          ports:
            - containerPort: 80
              protocol: TCP
          env:
            - name: LOG_LEVEL
              value: info
            - name: WORKERS
              value: \"4\"
          resources:
            limits:
              cpu: 500m
              memory: 128Mi
",
        typed: manifest,
    },
    Input {
        name: "k8s-configmap",
        src: "\
apiVersion: v1
kind: ConfigMap
metadata:
  name: settings
data:
  enabled: \"true\"
  retries: \"3\"
  script: |
    #!/bin/sh
    echo hello
",
        typed: manifest,
    },
    Input {
        name: "k8s-multi",
        src: "\
---
apiVersion: v1
kind: Service
metadata:
  name: web
spec:
  ports:
    - port: 80
      targetPort: 8080
---
apiVersion: v1
kind: Namespace
metadata:
  name: prod
...
",
        typed: none,
    },
    Input {
        name: "gha-workflow",
        src: "\
name: CI
on:
  push:
    branches: [main]
  pull_request:
env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
        rust: [stable, beta, 1.85]
    steps:
      - uses: actions/checkout@v4
      - name: Setup Python
        uses: actions/setup-python@v5
        with:
          python-version: 3.10
          cache: pip
      - name: Test
        run: |
          cargo test --workspace
          cargo clippy -- -D warnings
",
        typed: workflow,
    },
    Input {
        name: "gha-on-list",
        src: "\
name: Release
on: [push, workflow_dispatch]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: make
",
        typed: workflow,
    },
    Input {
        name: "compose-merge",
        src: "\
version: \"3.8\"
x-common: &common
  restart: unless-stopped
  environment:
    TZ: UTC
    LOG_LEVEL: info
services:
  web:
    <<: *common
    image: nginx:1.27
    ports:
      - \"8080:80\"
      - 443:443
  api:
    <<: *common
    image: example/api:2.3
    restart: always
",
        typed: compose,
    },
    Input {
        name: "compose-merge-list",
        src: "\
x-a: &a
  restart: always
  environment:
    A: 1
x-b: &b
  image: busybox
  restart: \"no\"
services:
  one:
    <<: [*a, *b]
  two:
    <<: [*b, *a]
    environment:
      B: 2
",
        typed: compose,
    },
    Input {
        name: "compose-ports",
        src: "\
services:
  db:
    image: postgres:16
    ports:
      - 5432:5432
      - 22:22
      - 1:30
    environment:
      POSTGRES_PASSWORD: example
      PGPORT: 5432
",
        typed: compose,
    },
    Input {
        name: "config-app",
        src: "\
name: my-service
port: 8080
debug: false
ratio: 0.75
version: 1.10
tags: [api, internal]
database:
  url: postgres://localhost/db
  pool: 16
  timeout: 30s
",
        typed: app,
    },
    Input {
        name: "config-yes-no",
        src: "\
name: legacy
port: 80
enabled: yes
verbose: no
feature: on
country: NO
answer: y
",
        typed: strings,
    },
    Input {
        name: "config-yes-bool",
        src: "name: legacy\nport: 80\ndebug: yes\n",
        typed: app,
    },
    Input {
        name: "config-octal",
        src: "name: files\nport: 21\nmode: 0644\n",
        typed: app,
    },
    Input {
        name: "config-octal-12",
        src: "name: files\nport: 21\nmode: 0o644\n",
        typed: app,
    },
    Input {
        name: "config-int-forms",
        src: "a: 0x1F\nb: 0b1010\nc: -0x10\nd: +0x10\ne: 0o17\nf: -0o17\ng: +12\nh: 1_000\n",
        typed: none,
    },
    Input {
        name: "config-binary",
        src: "name: bits\nport: 1\nmode: 0b1010\n",
        typed: app,
    },
    Input {
        name: "config-sexagesimal",
        src: "time: 12:30\nduration: 1:30:00\nport: 22:22\n",
        typed: strings,
    },
    Input {
        name: "config-floats",
        src: "a: 1.0\nb: .5\nc: 1e3\nd: -.inf\ne: .NaN\nf: 1e400\ng: 6.02e+23\nh: 1.\n",
        typed: none,
    },
    Input {
        name: "config-wide-ints",
        src: "a: 9223372036854775807\nb: 18446744073709551615\nc: 18446744073709551616\nd: -9223372036854775809\ne: 340282366920938463463374607431768211456\n",
        typed: numbers,
    },
    Input {
        name: "config-nulls",
        src: "a: ~\nb: null\nc: Null\nd:\ne: NULL\nf: nil\n",
        typed: none,
    },
    Input {
        name: "config-quoted-numbers",
        src: "name: q\nport: \"8080\"\n",
        typed: app,
    },
    Input {
        name: "config-string-coercion",
        src: "a: 8080\nb: 1.10\nc: true\nd: 0x1F\ne: ~\nf: 'x'\n",
        typed: strings,
    },
    Input {
        name: "config-duplicate-keys",
        src: "name: first\nport: 1\nname: second\n",
        typed: app,
    },
    Input {
        name: "config-duplicate-map-keys",
        src: "a: 1\nb: 2\na: 3\n",
        typed: strings,
    },
    Input {
        name: "config-timestamps",
        src: "date: 2001-12-14\nwhen: 2001-12-14t21:59:43.10-05:00\n",
        typed: strings,
    },
    Input {
        name: "config-int-keys",
        src: "1: one\n2: two\n010: ten\n",
        typed: int_map,
    },
    Input {
        name: "config-anchors",
        src: "\
defaults: &defaults
  url: postgres://localhost/db
  pool: 4
name: anchors
port: 9000
database: *defaults
",
        typed: app,
    },
    Input {
        name: "tags-custom",
        src: "\
secret: !vault path/to/secret
env: !env HOME
list: !join [a, b]
map: !ref {id: 1}
",
        typed: none,
    },
    Input {
        name: "tags-core",
        src: "a: !!str 123\nb: !!int \"42\"\nc: !!float 1\nd: !!bool true\ne: !!null ''\nf: !!binary aGVsbG8=\n",
        typed: none,
    },
    Input {
        name: "enum-tags",
        src: "\
shapes:
  - Unit
  - !Circle {r: 1.5}
  - !Square 2.0
  - !Pair [1, 2]
  - !Named hello
",
        typed: drawing,
    },
    Input {
        name: "enum-maps",
        src: "\
shapes:
  - Unit
  - Circle: {r: 1.5}
  - Square: 2.0
  - Pair: [1, 2]
  - Named: hello
",
        typed: drawing,
    },
    Input {
        name: "enum-internal-untagged",
        src: "\
sources:
  - type: Http
    url: https://example.com
  - type: File
    path: /etc/app.yaml
values: [1, two, [x, y], 0x10]
",
        typed: sources,
    },
    Input {
        name: "flatten",
        src: "name: f\nport: 80\nlist: [1, 2]\nnested: {a: null}\n",
        typed: flatten,
    },
    Input {
        name: "block-scalars",
        src: "\
literal: |
  line 1
  line 2
folded: >-
  folded
  text

  para
keep: |+
  kept

strip: |-
  stripped
indented: |2
    two extra
",
        typed: strings,
    },
    Input {
        name: "quoting",
        src: "\
single: 'it''s'
double: \"tab\\there \\u263A \\x41\"
url: http://example.com:8080/path?q=1#frag
colon: a:b
dash: - not a list
",
        typed: none,
    },
    Input {
        name: "quoting-ok",
        src: "\
single: 'it''s'
double: \"tab\\there \\u263A \\x41\"
url: http://example.com:8080/path?q=1#frag
colon: a:b
",
        typed: strings,
    },
    Input {
        name: "complex-keys",
        src: "? [a, b]\n: 1\n? {k: v}\n: 2\n",
        typed: none,
    },
    Input {
        name: "empty-document",
        src: "",
        typed: none,
    },
    Input {
        name: "comment-only",
        src: "# nothing here\n",
        typed: none,
    },
    Input {
        name: "multi-configs",
        src: "name: a\nport: 1\n---\nname: b\nport: 2\n",
        typed: none,
    },
    Input {
        name: "yaml-1-1-directive",
        src: "%YAML 1.1\n---\nenabled: yes\nmode: 0644\n",
        typed: strings,
    },
    Input {
        name: "tabs-in-values",
        src: "a:\tb\nc: d\t# comment\n",
        typed: strings,
    },
    Input {
        name: "inline-colon",
        src: "title: The farm: a banana story\n",
        typed: strings,
    },
];
