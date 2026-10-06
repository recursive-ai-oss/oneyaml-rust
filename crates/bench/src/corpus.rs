//! Deterministic, license-clean benchmark corpus.
//!
//! Every input is **generated** by the code in this file, so the corpus is
//! versioned with the crate and contains no third-party text. The real-world
//! shapes (Kubernetes, GitHub Actions, docker-compose, OpenAPI) are written
//! from scratch to follow the public structure of those formats; the stress
//! shapes are synthetic. Generators take a target size in bytes and repeat or
//! deepen a template until the output reaches it, so the same generator gives
//! the 1×/10×/100× inputs of the linear-scaling check.

use std::fmt::Write as _;

/// Which half of the corpus a case belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// Real-world style documents at a fixed size.
    RealWorld,
    /// Synthetic stress shapes, measured at 1×, 10× and 100× of `base`.
    Stress,
}

/// One corpus entry.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    /// Stable name used on the command line and in the tables.
    pub name: &'static str,
    /// Real-world or stress.
    pub kind: Kind,
    /// Target size in bytes at scale 1.
    pub base: usize,
    /// What the input exercises.
    pub about: &'static str,
    generate: fn(usize) -> String,
}

impl Case {
    /// Generates the input at `scale` × [`Case::base`] bytes (saturating).
    #[must_use]
    pub fn generate(&self, scale: usize) -> String {
        (self.generate)(self.base.saturating_mul(scale))
    }
}

const KB: usize = 1024;
const MB: usize = 1024 * 1024;

/// The whole corpus, real-world first.
pub const CASES: &[Case] = &[
    case(
        Kind::RealWorld,
        "k8s-manifest",
        5 * KB,
        "Deployment + Service + ConfigMap + Ingress, one set",
        k8s,
    ),
    case(
        Kind::RealWorld,
        "gha-workflow",
        3 * KB,
        "GitHub Actions workflow: matrix, steps, literal-block `run` scripts",
        gha,
    ),
    case(
        Kind::RealWorld,
        "compose-merge",
        2 * KB,
        "docker-compose with `x-` anchors and `<<: *` merge keys",
        compose,
    ),
    case(
        Kind::RealWorld,
        "k8s-multi",
        MB,
        "~300 `---`-separated Kubernetes documents",
        k8s,
    ),
    case(
        Kind::RealWorld,
        "openapi-100k",
        100 * KB,
        "OpenAPI-style spec: paths, schemas, `$ref`s",
        openapi,
    ),
    case(
        Kind::RealWorld,
        "openapi-10m",
        10 * MB,
        "the same OpenAPI shape at ~10 MB",
        openapi,
    ),
    case(
        Kind::RealWorld,
        HELM_VALUES,
        100 * KB,
        "Helm umbrella-chart values: long `# --` comment runs, commented-out blocks, comments under `{}`/`[]`",
        helm_values,
    ),
    case(
        Kind::Stress,
        "deep-block",
        10 * KB,
        "one chain of nested block mappings (depth = sqrt(2 × size))",
        deep_block,
    ),
    case(
        Kind::Stress,
        "deep-flow",
        10 * KB,
        "`[[[...]]]` nested flow sequences (depth = size/2)",
        deep_flow,
    ),
    case(
        Kind::Stress,
        "wide-map",
        10 * KB,
        "one flat block mapping with many keys",
        wide_map,
    ),
    case(
        Kind::Stress,
        "long-scalars",
        10 * KB,
        "four huge scalars: plain, single, double (escapes), multi-line",
        long_scalars,
    ),
    case(
        Kind::Stress,
        "block-scalars",
        10 * KB,
        "many literal and folded scalars, every chomping indicator",
        block_scalars,
    ),
    case(
        Kind::Stress,
        "flow-heavy",
        10 * KB,
        "block sequence of nested flow mappings/sequences",
        flow_heavy,
    ),
    case(
        Kind::Stress,
        "comment-heavy",
        10 * KB,
        "comment blocks and trailing comments on every line",
        comment_heavy,
    ),
    case(
        Kind::Stress,
        "anchors",
        10 * KB,
        "many anchors, aliases and `<<` merges",
        anchors,
    ),
    case(
        Kind::Stress,
        "whitespace",
        10 * KB,
        "blank lines, trailing spaces, wide indentation",
        whitespace,
    ),
];

const fn case(
    kind: Kind,
    name: &'static str,
    base: usize,
    about: &'static str,
    generate: fn(usize) -> String,
) -> Case {
    Case {
        name,
        kind,
        base,
        about,
        generate,
    }
}

/// The Helm-values case: the corpus input whose comment runs
/// are long enough for the comment-run index of `-syntax`
/// to show in the `Document` and edit tables. Inputs recorded before it
/// existed (the golden baselines of `tests/doc_diagnostics.rs` and
/// `tests/ser_differential.rs`) skip it by this name.
pub const HELM_VALUES: &str = "helm-values";

/// Looks a case up by name.
#[must_use]
pub fn find(name: &str) -> Option<&'static Case> {
    CASES.iter().find(|c| c.name == name)
}

/// FNV-1a 64 of `bytes`, printed in the corpus table so a changed generator
/// is visible.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Appends `block(i)` for i = 0, 1, … until `out` reaches `target` bytes.
fn fill(target: usize, mut block: impl FnMut(&mut String, usize)) -> String {
    let mut out = String::with_capacity(target.saturating_add(4 * KB));
    let mut i = 0usize;
    while out.len() < target {
        block(&mut out, i);
        i = i.saturating_add(1);
    }
    out
}

/// A small deterministic word source (no RNG dependency).
fn word(i: usize) -> &'static str {
    const WORDS: &[&str] = &[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima", "mike", "november", "oscar", "papa",
    ];
    WORDS
        .get(
            i.wrapping_mul(7)
                .wrapping_add(3)
                .checked_rem(WORDS.len())
                .unwrap_or(0),
        )
        .copied()
        .unwrap_or("x")
}

fn k8s(target: usize) -> String {
    fill(target, |s, i| {
        let n = word(i);
        let _ = write!(
            s,
            "---\n# Source: {n}/templates/deployment.yaml\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: {n}-{i}\n  namespace: prod\n  labels:\n    app.kubernetes.io/name: {n}\n    app.kubernetes.io/instance: \"{n}-{i}\"\n  annotations:\n    deployment.kubernetes.io/revision: \"{i}\"\nspec:\n  replicas: 3\n  selector:\n    matchLabels:\n      app: {n}\n  strategy:\n    type: RollingUpdate\n    rollingUpdate: {{maxSurge: 25%, maxUnavailable: 0}}\n  template:\n    metadata:\n      labels:\n        app: {n}\n    spec:\n      containers:\n        - name: {n}\n          image: \"registry.example.com/{n}:1.{i}.0\"\n          imagePullPolicy: IfNotPresent\n          args: [\"--port=8080\", \"--log-level=info\"]\n          ports:\n            - name: http\n              containerPort: 8080\n              protocol: TCP\n          env:\n            - name: DATABASE_URL\n              valueFrom:\n                secretKeyRef: {{name: {n}-db, key: url}}\n            - name: CACHE_TTL\n              value: '300'\n          resources:\n            limits: {{cpu: 500m, memory: 512Mi}}\n            requests: {{cpu: 100m, memory: 128Mi}}\n          livenessProbe:\n            httpGet: {{path: /healthz, port: http}}\n            initialDelaySeconds: 10\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: {n}-{i}\nspec:\n  type: ClusterIP\n  selector: {{app: {n}}}\n  ports:\n    - port: 80\n      targetPort: http\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {n}-{i}-config\ndata:\n  app.properties: |\n    server.port=8080\n    feature.{n}=true\n  LOG_FORMAT: json\n---\napiVersion: networking.k8s.io/v1\nkind: Ingress\nmetadata:\n  name: {n}-{i}\n  annotations:\n    nginx.ingress.kubernetes.io/rewrite-target: /\nspec:\n  rules:\n    - host: {n}.example.com\n      http:\n        paths:\n          - path: /\n            pathType: Prefix\n            backend:\n              service:\n                name: {n}-{i}\n                port: {{number: 80}}\n"
        );
    })
}

fn gha(target: usize) -> String {
    let mut s = String::from(
        "name: CI\n\non:\n  push:\n    branches: [main]\n  pull_request:\n    paths-ignore: ['docs/**', '*.md']\n\nenv:\n  CARGO_TERM_COLOR: always\n  RUSTFLAGS: \"-D warnings\"\n\njobs:\n",
    );
    let rest = fill(target.saturating_sub(s.len()), |s, i| {
        let n = word(i);
        let _ = write!(
            s,
            "  {n}-{i}:\n    name: ${{{{ matrix.os }}}} / {n}\n    runs-on: ${{{{ matrix.os }}}}\n    strategy:\n      fail-fast: false\n      matrix:\n        os: [ubuntu-latest, macos-latest, windows-latest]\n        toolchain: [stable, beta, \"1.85\"]\n    steps:\n      - uses: actions/checkout@v4\n      - name: Install toolchain\n        uses: dtolnay/rust-toolchain@master\n        with:\n          toolchain: ${{{{ matrix.toolchain }}}}\n          components: clippy, rustfmt\n      - name: Cache\n        uses: actions/cache@v4\n        with:\n          path: |\n            ~/.cargo/registry\n            target\n          key: ${{{{ runner.os }}}}-cargo-${{{{ hashFiles('**/Cargo.lock') }}}}\n      - name: Test\n        run: |\n          cargo build --workspace --all-targets\n          cargo test --workspace -- --nocapture\n        # the suite needs the vendored data\n        env:\n          RUST_BACKTRACE: 1\n      - if: github.event_name == 'push'\n        run: >-\n          cargo doc --no-deps\n          --workspace\n"
        );
    });
    s.push_str(&rest);
    s
}

fn compose(target: usize) -> String {
    let mut s = String::from(
        "version: \"3.9\"\n\nx-common: &common\n  restart: unless-stopped\n  logging:\n    driver: json-file\n    options: {max-size: \"10m\", max-file: \"3\"}\n  networks: [backend]\n\nx-env: &env\n  TZ: UTC\n  LOG_LEVEL: info\n\nservices:\n",
    );
    let rest = fill(target.saturating_sub(s.len()), |s, i| {
        let n = word(i);
        let _ = write!(
            s,
            "  {n}{i}:\n    <<: *common\n    image: \"example/{n}:latest\"\n    environment:\n      <<: *env\n      SERVICE: {n}\n    ports:\n      - \"80{i:02}:80\"\n    depends_on: [db]\n    healthcheck:\n      test: [\"CMD\", \"curl\", \"-f\", \"http://localhost/health\"]\n      interval: 30s\n"
        );
    });
    s.push_str(&rest);
    s.push_str(
        "  db:\n    <<: *common\n    image: postgres:16\n    volumes:\n      - db-data:/var/lib/postgresql/data\n\nnetworks:\n  backend: {}\nvolumes:\n  db-data:\n",
    );
    s
}

fn openapi(target: usize) -> String {
    let mut s = String::from(
        "openapi: 3.1.0\ninfo:\n  title: Example API\n  version: 1.0.0\n  description: >\n    A generated API description used only for benchmarking.\n    It follows the OpenAPI document structure.\nservers:\n  - url: https://api.example.com/v1\npaths:\n",
    );
    // Paths take ~2/3 of the bytes, component schemas the rest.
    let paths_target = target.saturating_sub(s.len()).saturating_mul(2) / 3;
    s.push_str(&fill(paths_target, |s, i| {
        let n = word(i);
        let _ = write!(
            s,
            "  /{n}s/{i}/{{id}}:\n    parameters:\n      - name: id\n        in: path\n        required: true\n        schema: {{type: string, format: uuid}}\n    get:\n      operationId: get{n}{i}\n      summary: \"Fetch a {n} by id\"\n      tags: [{n}]\n      responses:\n        '200':\n          description: OK\n          content:\n            application/json:\n              schema:\n                $ref: '#/components/schemas/{n}{i}'\n        '404': {{$ref: '#/components/responses/NotFound'}}\n    delete:\n      operationId: delete{n}{i}\n      responses:\n        '204': {{description: Deleted}}\n"
        );
    }));
    s.push_str("components:\n  responses:\n    NotFound: {description: Not found}\n  schemas:\n");
    let rest = target.saturating_sub(s.len());
    s.push_str(&fill(rest, |s, i| {
        let n = word(i);
        let _ = write!(
            s,
            "    {n}{i}:\n      type: object\n      required: [id, name]\n      properties:\n        id: {{type: string, format: uuid, readOnly: true}}\n        name:\n          type: string\n          maxLength: 128\n          example: \"{n} number {i}\"\n        count: {{type: integer, minimum: 0, default: 0}}\n        ratio: {{type: number, example: 0.{i}}}\n        enabled: {{type: boolean, default: true}}\n"
        );
    }));
    s
}

/// A Helm umbrella chart's `values.yaml`: a long header comment, a
/// `global` section, then one section per sub-chart, each documented the
/// helm-docs way (`# --` lines), with commented-out example blocks under
/// `{}`/`[]` values (indented deeper than their key, as `helm create`
/// writes them) and a long comment run per section. The file ends in a
/// comment block after the last section. Written for this crate.
fn helm_values(target: usize) -> String {
    let mut s = String::from(
        "# Default values for the umbrella chart.\n# This is a YAML-formatted file.\n# Declare variables to be passed into your templates.\n#\n# Every sub-chart below can be switched off with `<name>.enabled: false`.\n# Values set here override the sub-chart's own defaults; values passed\n# with --set or -f override these in turn.\n#\n# Conventions used in this file:\n#   - lines starting with `# --` document the value below them;\n#   - commented-out blocks show optional settings with their defaults;\n#   - sizes use Kubernetes quantities (100m, 128Mi).\n\nglobal:\n  # -- Registry prepended to every image repository below.\n  imageRegistry: \"\"\n  # -- Secrets used to pull images from a private registry.\n  imagePullSecrets: []\n  #  - name: registry-credentials\n  # -- Storage class for every persistent volume claim.\n  storageClass: standard\n\n",
    );
    let body = fill(
        target.saturating_sub(s.len()).saturating_sub(400),
        |s, i| {
            let n = word(i);
            let _ = write!(
                s,
                "# ---------------------------------------------------------------\n# {n}{i}: the {n} service\n# ---------------------------------------------------------------\n#\n# The {n} service answers requests on port 8080 inside the pod and\n# is exposed through a ClusterIP service on port 80. It needs the\n# database credentials secret of the release; see the chart README\n# for how to create it before the first install.\n#\n# Upgrading from an older release: the image tag moved from\n# `{n}.tag` to `{n}.image.tag`, and `{n}.port` became\n# `{n}.service.port`. Both old keys are still read but are ignored\n# when the new ones are set.\n#\n{n}{i}:\n  # -- Deploy the {n} sub-chart.\n  enabled: true\n  # -- Number of pods when autoscaling is off.\n  replicaCount: 2\n\n  image:\n    # -- Image repository (the global registry is prepended).\n    repository: example/{n}\n    # -- Image pull policy.\n    pullPolicy: IfNotPresent\n    # -- Overrides the image tag, which defaults to the chart's appVersion.\n    tag: \"1.{i}.0\"\n\n  serviceAccount:\n    # -- Create a service account for the pods.\n    create: true\n    # -- Annotations for the service account.\n    annotations: {{}}\n    # -- Name of the service account. Generated from the release name\n    # when empty and `create` is true.\n    name: \"\"\n\n  # -- Annotations added to every pod.\n  podAnnotations: {{}}\n\n  podSecurityContext: {{}}\n    # fsGroup: 2000\n\n  securityContext: {{}}\n    # capabilities:\n    #   drop:\n    #   - ALL\n    # readOnlyRootFilesystem: true\n    # runAsNonRoot: true\n    # runAsUser: 1000\n\n  service:\n    # -- Service type.\n    type: ClusterIP\n    # -- Service port.\n    port: 80\n\n  ingress:\n    # -- Expose the service through an ingress.\n    enabled: false\n    className: \"\"\n    annotations: {{}}\n      # kubernetes.io/ingress.class: nginx\n      # kubernetes.io/tls-acme: \"true\"\n    hosts:\n      - host: {n}.example.local\n        paths:\n          - path: /\n            pathType: ImplementationSpecific\n    tls: []\n    #  - secretName: {n}-tls\n    #    hosts:\n    #      - {n}.example.local\n\n  resources: {{}}\n    # Resources are left to the operator on purpose: the right numbers\n    # depend on the traffic and on the cluster. To set them, remove the\n    # braces after `resources:` and uncomment the lines below, adjusting\n    # them as needed.\n    # limits:\n    #   cpu: 500m\n    #   memory: 512Mi\n    # requests:\n    #   cpu: 100m\n    #   memory: 128Mi\n\n  autoscaling:\n    # -- Scale the deployment with a HorizontalPodAutoscaler.\n    enabled: false\n    minReplicas: 1\n    maxReplicas: 10\n    targetCPUUtilizationPercentage: 80\n    # targetMemoryUtilizationPercentage: 80\n\n  # -- Node labels for pod assignment.\n  nodeSelector: {{}}\n\n  # -- Tolerations for pod assignment.\n  tolerations: []\n\n  # -- Affinity rules for pod assignment.\n  affinity: {{}}\n\n"
            );
        },
    );
    s.push_str(&body);
    s.push_str(
        "# ---------------------------------------------------------------\n# Add further sub-charts above this line. Each needs an entry in\n# Chart.yaml's dependencies with a condition of `<name>.enabled`.\n#\n# extra:\n#   enabled: false\n#   replicaCount: 1\n# ---------------------------------------------------------------\n",
    );
    s
}

fn deep_block(target: usize) -> String {
    // Line i is `i` spaces + `k:\n`, so the size is ~depth²/2; pick the
    // depth that lands near `target`.
    let depth = target.saturating_mul(2).isqrt().max(1);
    let mut s = String::with_capacity(target.saturating_add(depth.saturating_mul(8)));
    for i in 0..depth {
        s.extend(std::iter::repeat_n(' ', i));
        s.push_str("k:\n");
    }
    s.extend(std::iter::repeat_n(' ', depth));
    s.push_str("leaf\n");
    s
}

fn deep_flow(target: usize) -> String {
    let depth = (target / 2).max(1);
    let mut s = String::with_capacity(target.saturating_add(4));
    s.extend(std::iter::repeat_n('[', depth));
    s.push('x');
    s.extend(std::iter::repeat_n(']', depth));
    s.push('\n');
    s
}

fn wide_map(target: usize) -> String {
    fill(target, |s, i| {
        let _ = writeln!(s, "key_{i:07}: {}", word(i));
    })
}

fn long_scalars(target: usize) -> String {
    let part = (target / 4).max(1);
    let words = |sep: &str| {
        fill(part, |s, i| {
            if i > 0 {
                s.push_str(sep);
            }
            s.push_str(word(i));
        })
    };
    let mut s = String::with_capacity(target.saturating_add(64));
    let _ = writeln!(s, "plain: {}", words(" "));
    let _ = writeln!(s, "single: '{}'", words(" it''s "));
    let _ = writeln!(s, "double: \"{}\"", words(" \\t\\u00e9\\\" "));
    let _ = writeln!(s, "multi: {}", words("\n  "));
    s
}

fn block_scalars(target: usize) -> String {
    const HEADS: &[&str] = &["|", "|-", "|+", ">", ">-", ">+", "|2", ">1-"];
    fill(target, |s, i| {
        let head = HEADS
            .get(i.checked_rem(HEADS.len()).unwrap_or(0))
            .copied()
            .unwrap_or("|");
        let _ = writeln!(s, "text_{i}: {head}");
        for line in 0..12 {
            let n = word(i.wrapping_add(line));
            let _ = writeln!(s, "   {n} line {line} of block {i}");
            if line == 5 {
                s.push_str("\n     more-indented {\n");
            }
        }
    })
}

fn flow_heavy(target: usize) -> String {
    fill(target, |s, i| {
        let n = word(i);
        let _ = writeln!(
            s,
            "- {{name: {n}, id: {i}, tags: [a, b, \"c d\", 'e'], nested: {{k: [1, 2, {{z: 3, y: [x, {{w: v}}]}}], empty: [], none: {{}}}}, ? complex : key}}"
        );
    })
}

fn comment_heavy(target: usize) -> String {
    fill(target, |s, i| {
        let n = word(i);
        let _ = write!(
            s,
            "# ---------------------------------------------\n# Section {i}: {n}\n#   explanatory text that is longer than the data\n\nsection_{i}:  # trailing comment on the key\n  # leading comment on the first entry\n  a: 1   # one\n  b: {n} # two\n  #   indented comment\n  c:\n    - x  # item\n    # between items\n    - y\n"
        );
    })
}

fn anchors(target: usize) -> String {
    fill(target, |s, i| {
        let _ = writeln!(s, "a{i}: &a{i} {{x: {i}, y: {}}}", word(i));
        let _ = writeln!(s, "b{i}: &b{i}\n  <<: *a{i}\n  z: *a{i}");
        let prev = i.saturating_sub(1);
        let _ = writeln!(s, "c{i}: [*a{i}, *b{prev}, *a{prev}]");
    })
}

fn whitespace(target: usize) -> String {
    fill(target, |s, i| {
        let _ = write!(
            s,
            "\n\n    \nk{i}:   \n\n        -     v{i}   \n\n        -   \t w   \n    \n"
        );
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn names_unique_and_findable() {
        for c in CASES {
            assert_eq!(find(c.name).unwrap().name, c.name);
            assert_eq!(CASES.iter().filter(|d| d.name == c.name).count(), 1);
        }
        assert!(find("no-such-case").is_none());
    }

    #[test]
    fn deterministic() {
        for c in CASES.iter().filter(|c| c.base <= MB) {
            let a = c.generate(1);
            assert_eq!(
                fnv1a(a.as_bytes()),
                fnv1a(c.generate(1).as_bytes()),
                "{}",
                c.name
            );
        }
    }

    #[test]
    fn sizes_near_target() {
        for c in CASES.iter().filter(|c| c.base <= MB) {
            for scale in [1, 10] {
                let target = c.base * scale;
                let len = c.generate(scale).len();
                assert!(
                    len * 10 >= target * 8,
                    "{} @{scale}: {len} < {target}",
                    c.name
                );
                assert!(
                    len * 10 <= target * 13,
                    "{} @{scale}: {len} > {target}",
                    c.name
                );
            }
        }
    }

    #[test]
    fn fnv_known_value() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
