//! Typed serde targets for the `serde` sections: realistic derived structs
//! for each real-world corpus shape ("typed"), and sparse structs that read
//! a few fields and skip the rest ("sparse").
//!
//! They are written the way an application would write them: `String`
//! fields, `Option` for what may be absent, `#[serde(default)]`,
//! `#[serde(rename)]`, maps as `BTreeMap`, an internally tagged enum for
//! Kubernetes kinds and an untagged int-or-string. Unknown fields (e.g.
//! compose's `x-` extensions) are ignored. Every type also derives
//! `Serialize`, so the correctness guard can compare any two libraries'
//! results as `serde_json::Value`s.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

type Map<V = String> = BTreeMap<String, V>;

/// Kubernetes objects (k8s-manifest, k8s-multi), one per document.
pub mod k8s {
    use super::{Deserialize, Map, Serialize};

    /// One Kubernetes object, tagged by `kind`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "kind")]
    pub enum Object {
        /// `apps/v1` Deployment.
        Deployment {
            /// `apiVersion`.
            #[serde(rename = "apiVersion")]
            api_version: String,
            /// Object metadata.
            metadata: Meta,
            /// Deployment spec.
            spec: DeploymentSpec,
        },
        /// `v1` Service.
        Service {
            /// `apiVersion`.
            #[serde(rename = "apiVersion")]
            api_version: String,
            /// Object metadata.
            metadata: Meta,
            /// Service spec.
            spec: ServiceSpec,
        },
        /// `v1` ConfigMap.
        ConfigMap {
            /// `apiVersion`.
            #[serde(rename = "apiVersion")]
            api_version: String,
            /// Object metadata.
            metadata: Meta,
            /// String data.
            #[serde(default)]
            data: Map,
        },
        /// `networking.k8s.io/v1` Ingress.
        Ingress {
            /// `apiVersion`.
            #[serde(rename = "apiVersion")]
            api_version: String,
            /// Object metadata.
            metadata: Meta,
            /// Ingress spec.
            spec: IngressSpec,
        },
    }

    /// `metadata`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Meta {
        /// Name.
        pub name: String,
        /// Namespace.
        pub namespace: Option<String>,
        /// Labels.
        #[serde(default)]
        pub labels: Map,
        /// Annotations.
        #[serde(default)]
        pub annotations: Map,
    }

    /// Kubernetes `IntOrString`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum IntOrString {
        /// A number.
        Int(i64),
        /// A string (e.g. `25%`, or a named port).
        Str(String),
    }

    /// Deployment `spec`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DeploymentSpec {
        /// Replicas.
        pub replicas: Option<i32>,
        /// Label selector.
        pub selector: Selector,
        /// Rollout strategy.
        pub strategy: Option<Strategy>,
        /// Pod template.
        pub template: PodTemplate,
    }

    /// Label selector.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Selector {
        /// `matchLabels`.
        #[serde(default)]
        pub match_labels: Map,
    }

    /// Rollout strategy.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Strategy {
        /// `type`.
        #[serde(rename = "type")]
        pub kind: String,
        /// Rolling-update parameters.
        pub rolling_update: Option<Map<IntOrString>>,
    }

    /// Pod template.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PodTemplate {
        /// Template metadata (labels only).
        pub metadata: Option<TemplateMeta>,
        /// Pod spec.
        pub spec: PodSpec,
    }

    /// Template metadata.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct TemplateMeta {
        /// Labels.
        #[serde(default)]
        pub labels: Map,
    }

    /// Pod spec.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PodSpec {
        /// Containers.
        pub containers: Vec<Container>,
    }

    /// A container.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Container {
        /// Name.
        pub name: String,
        /// Image.
        pub image: String,
        /// Pull policy.
        pub image_pull_policy: Option<String>,
        /// Arguments.
        #[serde(default)]
        pub args: Vec<String>,
        /// Ports.
        #[serde(default)]
        pub ports: Vec<ContainerPort>,
        /// Environment.
        #[serde(default)]
        pub env: Vec<EnvVar>,
        /// Resources: `limits`/`requests` → resource → quantity.
        pub resources: Option<Map<Map>>,
        /// Liveness probe.
        pub liveness_probe: Option<Probe>,
    }

    /// A container port.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ContainerPort {
        /// Name.
        pub name: Option<String>,
        /// Port number.
        pub container_port: u16,
        /// Protocol.
        pub protocol: Option<String>,
    }

    /// An environment variable.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct EnvVar {
        /// Name.
        pub name: String,
        /// Literal value.
        pub value: Option<String>,
        /// `valueFrom`: source → key selector.
        pub value_from: Option<Map<Map>>,
    }

    /// A probe.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Probe {
        /// HTTP GET action.
        pub http_get: Option<Map<IntOrString>>,
        /// Initial delay.
        pub initial_delay_seconds: Option<i32>,
    }

    /// Service spec.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ServiceSpec {
        /// `type`.
        #[serde(rename = "type")]
        pub kind: Option<String>,
        /// Selector.
        #[serde(default)]
        pub selector: Map,
        /// Ports.
        pub ports: Vec<ServicePort>,
    }

    /// A service port.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ServicePort {
        /// Port.
        pub port: u16,
        /// Target port.
        pub target_port: Option<IntOrString>,
    }

    /// Ingress spec.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct IngressSpec {
        /// Rules.
        pub rules: Vec<IngressRule>,
    }

    /// An ingress rule.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct IngressRule {
        /// Host.
        pub host: Option<String>,
        /// HTTP paths.
        pub http: HttpRule,
    }

    /// `http`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct HttpRule {
        /// Paths.
        pub paths: Vec<HttpPath>,
    }

    /// One HTTP path.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct HttpPath {
        /// Path.
        pub path: String,
        /// Path type.
        pub path_type: String,
        /// Backend service.
        pub backend: Backend,
    }

    /// Ingress backend.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Backend {
        /// Service.
        pub service: BackendService,
    }

    /// Backend service.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct BackendService {
        /// Name.
        pub name: String,
        /// Port (`number` or `name`).
        pub port: Map<IntOrString>,
    }

    /// Sparse: kind and name only.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Sparse {
        /// Kind.
        pub kind: String,
        /// Metadata.
        pub metadata: SparseMeta,
    }

    /// Sparse metadata.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct SparseMeta {
        /// Name.
        pub name: String,
    }
}

/// GitHub Actions workflow (gha-workflow).
pub mod gha {
    use super::{Deserialize, Map, Serialize};

    /// A workflow.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Workflow {
        /// Name.
        pub name: String,
        /// Triggers: event → filters (branches, paths, …) → patterns.
        pub on: Map<Map<Vec<String>>>,
        /// Workflow environment.
        #[serde(default)]
        pub env: Map,
        /// Jobs by id.
        pub jobs: Map<Job>,
    }

    /// A job.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct Job {
        /// Display name.
        pub name: Option<String>,
        /// Runner label.
        pub runs_on: String,
        /// Matrix strategy.
        pub strategy: Option<Strategy>,
        /// Steps.
        pub steps: Vec<Step>,
    }

    /// Matrix strategy.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct Strategy {
        /// `fail-fast`.
        #[serde(default)]
        pub fail_fast: bool,
        /// Matrix axes.
        pub matrix: Map<Vec<String>>,
    }

    /// A step.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Step {
        /// Condition.
        #[serde(rename = "if")]
        pub condition: Option<String>,
        /// Display name.
        pub name: Option<String>,
        /// Action reference.
        pub uses: Option<String>,
        /// Action inputs.
        pub with: Option<Map>,
        /// Shell script.
        pub run: Option<String>,
        /// Step environment (`RUST_BACKTRACE: 1` reads as the string "1").
        pub env: Option<Map>,
    }

    /// Sparse: the workflow name and each job's runner.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Sparse {
        /// Name.
        pub name: String,
        /// Jobs by id.
        pub jobs: Map<SparseJob>,
    }

    /// Sparse job.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct SparseJob {
        /// Runner label.
        #[serde(rename = "runs-on")]
        pub runs_on: String,
    }
}

/// docker-compose file with merge keys (compose-merge).
pub mod compose {
    use super::{Deserialize, Map, Serialize};

    /// A compose file (`x-` extension fields are ignored).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Compose {
        /// File format version.
        pub version: String,
        /// Services.
        pub services: Map<Service>,
        /// Networks.
        #[serde(default)]
        pub networks: Map<Map>,
        /// Named volumes (may be null).
        #[serde(default)]
        pub volumes: Map<Option<Map>>,
    }

    /// A service; `restart`, `logging` and `networks` come from `<<: *common`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Service {
        /// Image.
        pub image: String,
        /// Restart policy.
        pub restart: Option<String>,
        /// Logging.
        pub logging: Option<Logging>,
        /// Networks.
        #[serde(default)]
        pub networks: Vec<String>,
        /// Environment (partly from `<<: *env`).
        #[serde(default)]
        pub environment: Map,
        /// Published ports.
        #[serde(default)]
        pub ports: Vec<String>,
        /// Dependencies.
        #[serde(default)]
        pub depends_on: Vec<String>,
        /// Health check.
        pub healthcheck: Option<Healthcheck>,
        /// Volume mounts.
        #[serde(default)]
        pub volumes: Vec<String>,
    }

    /// Logging.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Logging {
        /// Driver.
        pub driver: String,
        /// Options.
        #[serde(default)]
        pub options: Map,
    }

    /// Health check.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Healthcheck {
        /// Command.
        pub test: Vec<String>,
        /// Interval.
        pub interval: Option<String>,
    }

    /// Sparse: each service's image and restart policy (the latter merged).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Sparse {
        /// Services.
        pub services: Map<SparseService>,
    }

    /// Sparse service.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct SparseService {
        /// Image.
        pub image: String,
        /// Restart policy (from the merge).
        pub restart: Option<String>,
    }
}

/// Helm umbrella-chart values (helm-values): a `global` section
/// and one section per sub-chart, flattened into a map.
pub mod helm {
    use super::{Deserialize, Map, Serialize};

    /// The values file.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Values {
        /// Values shared by every sub-chart.
        pub global: Global,
        /// The sub-charts, by name.
        #[serde(flatten)]
        pub charts: Map<Chart>,
    }

    /// `global`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Global {
        /// Registry prefix.
        #[serde(rename = "imageRegistry")]
        pub image_registry: String,
        /// Pull secrets.
        #[serde(rename = "imagePullSecrets", default)]
        pub image_pull_secrets: Vec<Map>,
        /// Storage class.
        #[serde(rename = "storageClass")]
        pub storage_class: Option<String>,
    }

    /// One sub-chart's values.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Chart {
        /// Deploy it.
        pub enabled: bool,
        /// Replicas.
        #[serde(rename = "replicaCount")]
        pub replica_count: u32,
        /// Image.
        pub image: Image,
        /// Service account.
        #[serde(rename = "serviceAccount")]
        pub service_account: Option<ServiceAccount>,
        /// Pod annotations.
        #[serde(rename = "podAnnotations", default)]
        pub pod_annotations: Map,
        /// Pod security context.
        #[serde(rename = "podSecurityContext", default)]
        pub pod_security_context: Map<u32>,
        /// Container security context.
        #[serde(rename = "securityContext", default)]
        pub security_context: Map<bool>,
        /// Service.
        pub service: Service,
        /// Ingress.
        pub ingress: Option<Ingress>,
        /// Resources.
        #[serde(default)]
        pub resources: Map<Map>,
        /// Autoscaling.
        pub autoscaling: Option<Autoscaling>,
        /// Node selector.
        #[serde(rename = "nodeSelector", default)]
        pub node_selector: Map,
        /// Tolerations.
        #[serde(default)]
        pub tolerations: Vec<Map>,
        /// Affinity.
        #[serde(default)]
        pub affinity: Map<Map>,
    }

    /// Image.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Image {
        /// Repository.
        pub repository: String,
        /// Pull policy.
        #[serde(rename = "pullPolicy")]
        pub pull_policy: String,
        /// Tag.
        pub tag: String,
    }

    /// Service account.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct ServiceAccount {
        /// Create it.
        pub create: bool,
        /// Annotations.
        #[serde(default)]
        pub annotations: Map,
        /// Name.
        pub name: String,
    }

    /// Service.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Service {
        /// Type.
        #[serde(rename = "type")]
        pub kind: String,
        /// Port.
        pub port: u16,
    }

    /// Ingress.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Ingress {
        /// Enabled.
        pub enabled: bool,
        /// Class.
        #[serde(rename = "className")]
        pub class_name: String,
        /// Annotations.
        #[serde(default)]
        pub annotations: Map,
        /// Hosts.
        #[serde(default)]
        pub hosts: Vec<Host>,
        /// TLS.
        #[serde(default)]
        pub tls: Vec<Map>,
    }

    /// Ingress host.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Host {
        /// Host name.
        pub host: String,
        /// Paths.
        pub paths: Vec<Path>,
    }

    /// Ingress path.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Path {
        /// Path.
        pub path: String,
        /// Path type.
        #[serde(rename = "pathType")]
        pub path_type: String,
    }

    /// Autoscaling.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Autoscaling {
        /// Enabled.
        pub enabled: bool,
        /// Minimum replicas.
        #[serde(rename = "minReplicas")]
        pub min_replicas: u32,
        /// Maximum replicas.
        #[serde(rename = "maxReplicas")]
        pub max_replicas: u32,
        /// CPU target.
        #[serde(rename = "targetCPUUtilizationPercentage")]
        pub target_cpu: Option<u32>,
    }

    /// Sparse: the global registry and storage class (sub-charts skipped).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Sparse {
        /// Global values.
        pub global: SparseGlobal,
    }

    /// Sparse global.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct SparseGlobal {
        /// Registry prefix.
        #[serde(rename = "imageRegistry")]
        pub image_registry: String,
        /// Storage class.
        #[serde(rename = "storageClass")]
        pub storage_class: Option<String>,
    }
}

/// OpenAPI 3.1 description (openapi-100k, openapi-10m).
pub mod openapi {
    use super::{Deserialize, Map, Serialize};

    /// The document.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct OpenApi {
        /// Version.
        pub openapi: String,
        /// Info.
        pub info: Info,
        /// Servers.
        #[serde(default)]
        pub servers: Vec<Server>,
        /// Paths.
        pub paths: Map<PathItem>,
        /// Components.
        pub components: Components,
    }

    /// `info`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Info {
        /// Title.
        pub title: String,
        /// API version.
        pub version: String,
        /// Description.
        pub description: Option<String>,
    }

    /// A server.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Server {
        /// URL.
        pub url: String,
    }

    /// A path item.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PathItem {
        /// Shared parameters.
        #[serde(default)]
        pub parameters: Vec<Parameter>,
        /// GET.
        pub get: Option<Operation>,
        /// DELETE.
        pub delete: Option<Operation>,
    }

    /// A parameter.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Parameter {
        /// Name.
        pub name: String,
        /// Location.
        #[serde(rename = "in")]
        pub location: String,
        /// Required.
        #[serde(default)]
        pub required: bool,
        /// Schema.
        pub schema: Option<Schema>,
    }

    /// An operation.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Operation {
        /// Id.
        pub operation_id: Option<String>,
        /// Summary.
        pub summary: Option<String>,
        /// Tags.
        #[serde(default)]
        pub tags: Vec<String>,
        /// Responses by status code.
        pub responses: Map<Response>,
    }

    /// A response or a `$ref` to one.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Response {
        /// `$ref`.
        #[serde(rename = "$ref")]
        pub reference: Option<String>,
        /// Description.
        pub description: Option<String>,
        /// Content by media type.
        pub content: Option<Map<MediaType>>,
    }

    /// A media type.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct MediaType {
        /// Schema.
        pub schema: Schema,
    }

    /// A schema or a `$ref` to one (nested through `properties`).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Schema {
        /// `$ref`.
        #[serde(rename = "$ref")]
        pub reference: Option<String>,
        /// `type`.
        #[serde(rename = "type")]
        pub kind: Option<String>,
        /// Format.
        pub format: Option<String>,
        /// Required properties.
        #[serde(default)]
        pub required: Vec<String>,
        /// Properties.
        #[serde(default)]
        pub properties: Map<Schema>,
        /// Maximum length.
        pub max_length: Option<u64>,
        /// Minimum.
        pub minimum: Option<f64>,
        /// Read-only.
        pub read_only: Option<bool>,
    }

    /// `components`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Components {
        /// Shared responses.
        #[serde(default)]
        pub responses: Map<Response>,
        /// Schemas.
        #[serde(default)]
        pub schemas: Map<Schema>,
    }

    /// Sparse: the version and `info` (paths and components skipped).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Sparse {
        /// Version.
        pub openapi: String,
        /// Info.
        pub info: SparseInfo,
    }

    /// Sparse info.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct SparseInfo {
        /// Title.
        pub title: String,
        /// API version.
        pub version: String,
    }
}
