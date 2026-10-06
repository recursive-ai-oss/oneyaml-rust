//! Real-world example: editing a Kubernetes manifest without
//! losing its comments.
//!
//! New to oneyaml? Start with `edit_config.rs`, the five-call version;
//! this one is the advanced tour (streams, handles, the comment rulebook).
//!
//! The file holds two documents (a Deployment and a Service), so it is read
//! as a [`Stream`] and the Deployment is edited through
//! [`Stream::document_mut`]. The edits a release engineer would make:
//!
//! 1. bump the container image tag (its trailing note stays);
//! 2. add an environment variable with a leading comment;
//! 3. remove the `http` container port, whose commented-out neighbour
//!    (`# - name: https`) belongs to the port *after* it and so stays;
//! 4. drop `replicas`, now managed by a HorizontalPodAutoscaler.
//!
//! Then the Service is annotated through its own [`DocumentMut`]: a
//! header above its `---`, a note on its port, a commented-out TLS port
//! as the footer of `ports`, and a blank line before `spec`. Each
//! document of the stream writes its own region of the file, so
//! the Service's text alone reads back with the same comments.
//!
//! The example also shows what the rulebook (`docs/comment-attachment.md`)
//! says about that commented-out block, and that removing the `metrics`
//! port, which owns it, takes it along — unless `remove_keep_comments`
//! is used; and how a reader that disagrees with the rulebook re-owns it
//! for itself (`attach_comment`, not written to the text).
//!
//! Run with `cargo run -p oneyaml --example k8s_manifest`; `cargo test -p
//! oneyaml` runs it as a test.

mod common;

use oneyaml::{
    CommentOwner, CommentRule, Deserializer, Document, DocumentMut, EditError, ReinsertError,
    Stream, Value, path,
};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const MANIFEST: &str = "\
# Web frontend: Deployment and Service.

apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    app: web
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
          image: registry.example.com/web:1.4.2 # pinned by release tooling
          ports:
            - name: http
              containerPort: 8080
            # TLS terminates at the ingress for now:
            # - name: https
            #   containerPort: 8443
            - name: metrics
              containerPort: 9090
          env:
            # Verbosity: debug, info, warn, error.
            - name: LOG_LEVEL
              value: info
---
apiVersion: v1
kind: Service
metadata:
  name: web
spec:
  selector:
    app: web
  ports:
    - port: 80
      targetPort: 8080 # the container's http port
";

/// The whole file after [`edit`]: only the edited lines differ.
const EDITED: &str = "\
# Web frontend: Deployment and Service.

apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    app: web
spec:
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
          image: registry.example.com/web:1.5.0 # pinned by release tooling
          ports:
            # TLS terminates at the ingress for now:
            # - name: https
            #   containerPort: 8443
            - name: metrics
              containerPort: 9090
          env:
            # Verbosity: debug, info, warn, error.
            - name: LOG_LEVEL
              value: info
            # Enables the new search page (WEB-231).
            - name: FEATURE_SEARCH
              value: 'on'
          imagePullPolicy: IfNotPresent
# Service: the stable address in front of the Deployment.
---
apiVersion: v1
kind: Service
metadata:
  name: web

spec:
  selector:
    app: web
  ports:
    - port: 80 # what the ingress calls
      targetPort: 8080 # the container's http port
    # - port: 443 once TLS moves here
";

/// Comment owners of the Deployment, before and after the edits: the
/// edits add one comment and move none.
const OWNERS_BEFORE: &[&str] = &[
    "# Web frontend: Deployment and Service. -> document header (rule 2)",
    "# pinned by release tooling -> trailing of `image` (rule 5)",
    "# TLS terminates at the ingress for now: -> leading of `name: metrics` (rule 4)",
    "# - name: https -> leading of `name: metrics` (rule 4)",
    "#   containerPort: 8443 -> leading of `name: metrics` (rule 4)",
    "# Verbosity: debug, info, warn, error. -> leading of `name: LOG_LEVEL` (rule 4)",
];
const OWNERS_AFTER: &[&str] = &[
    "# Web frontend: Deployment and Service. -> document header (rule 2)",
    "# pinned by release tooling -> trailing of `image` (rule 5)",
    "# TLS terminates at the ingress for now: -> leading of `name: metrics` (rule 4)",
    "# - name: https -> leading of `name: metrics` (rule 4)",
    "#   containerPort: 8443 -> leading of `name: metrics` (rule 4)",
    "# Verbosity: debug, info, warn, error. -> leading of `name: LOG_LEVEL` (rule 4)",
    "# Enables the new search page (WEB-231). -> leading of `name: FEATURE_SEARCH` (rule 4)",
];

/// A new environment variable; serialized by the edit API with the
/// serializer's every-reader-safe quoting (`on` is a boolean to YAML 1.1
/// readers, so it is written `'on'`).
#[derive(Serialize)]
struct EnvVar {
    name: &'static str,
    value: &'static str,
}

/// The part of the Deployment the typed read checks.
#[derive(Deserialize)]
struct Deployment {
    spec: DeploymentSpec,
}
#[derive(Deserialize)]
struct DeploymentSpec {
    template: PodTemplate,
}
#[derive(Deserialize)]
struct PodTemplate {
    spec: PodSpec,
}
#[derive(Deserialize)]
struct PodSpec {
    containers: Vec<Container>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Container {
    image: String,
    ports: Vec<Port>,
    env: Vec<Env>,
    image_pull_policy: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Port {
    name: String,
    container_port: u16,
}
#[derive(Deserialize)]
struct Env {
    name: String,
    value: String,
}

/// Edits the manifest and checks every claim the module docs make.
fn edit() -> Result<Stream> {
    let mut stream = Stream::parse(MANIFEST)?;
    assert_eq!(stream.len(), 2);
    let deployment = stream.document(0).ok_or("no Deployment")?;
    assert_eq!(common::owners(&deployment)?, OWNERS_BEFORE);

    // Why the commented-out port stays when `http` goes: rule 4 gives the
    // comment lines to the item *below* them.
    let https = deployment
        .comment_ids()
        .into_iter()
        .find(|&id| {
            deployment
                .explain_comment(id)
                .is_ok_and(|e| e.text() == "# - name: https")
        })
        .ok_or("no commented-out port")?;
    let why = deployment.explain_comment(https)?;
    let metrics = deployment
        .id_of(&path![
            "spec",
            "template",
            "spec",
            "containers",
            0,
            "ports",
            1
        ])
        .ok_or("no metrics port")?;
    assert_eq!(why.rule(), CommentRule::Leading);
    assert_eq!(why.owner(), CommentOwner::Leading(metrics));
    println!("rule {}: {}", why.rule().number(), why.rule().summary());

    // So removing `metrics` would take the block with it (the rulebook
    // owner goes with its node), while `remove_keep_comments` leaves it as
    // the footer of `ports`. Shown on a clone (O(1) until edited).
    {
        let mut scratch = deployment.clone();
        let removed = scratch.remove_node(metrics)?;
        assert_eq!(removed.comments().len(), 3);
        assert_eq!(removed.comment_ids().len(), 3);
        let mut scratch = deployment.clone();
        let removed = scratch.remove_keep_comments(metrics)?;
        assert!(removed.comments().is_empty());
        let ports = scratch
            .id_of(&path!["spec", "template", "spec", "containers", 0, "ports"])
            .ok_or("no ports")?;
        let footer = scratch.comments(ports)?;
        assert_eq!(footer.footer().len(), 3);
        let first = footer.footer().first().map(|c| c.text());
        assert_eq!(first, Some("TLS terminates at the ingress for now:"));
    }

    {
        let mut doc = stream.document_mut(0).ok_or("no Deployment")?;
        let container = path!["spec", "template", "spec", "containers", 0];

        // 1. Bump the image tag. The value is replaced; the entry keeps its
        //    same-line comment.
        let mut image = container.clone();
        image.push_key("image");
        doc.set_path(&image, "registry.example.com/web:1.5.0")?;

        // 2. Add an environment variable, with a comment above it: `push`
        //    returns a handle to the new item, so the comment goes on in
        //    the same chain.
        let mut env = container.clone();
        env.push_key("env");
        doc.get_path_mut(&env)?
            .push(&EnvVar {
                name: "FEATURE_SEARCH",
                value: "on",
            })?
            .set_leading(["Enables the new search page (WEB-231)."])?;

        // `entry` inserts only what is missing (here: appended last).
        let pull = doc.get_path_mut(&container)?.entry("imagePullPolicy")?;
        assert!(!pull.is_occupied());
        pull.or_insert("IfNotPresent")?;

        // 3. Remove the `http` port. Its neighbour's comment lines are not
        //    its own (rule 4), so the returned `Removed` has none.
        let mut ports = container.clone();
        ports.push_key("ports");
        let http = doc.get_path_mut(&ports)?.remove_at(0)?;
        assert_eq!(http.text(), "name: http\n              containerPort: 8080");
        assert!(http.comments().is_empty());

        // 4. `replicas` goes; nothing else in `spec` changes.
        let replicas = doc.get_mut("spec")?.remove("replicas")?;
        assert_eq!(replicas.text(), "replicas: 3");
    }
    assert!(!stream.has_errors());

    let deployment = stream.document(0).ok_or("no Deployment")?;
    assert_eq!(common::owners(&deployment)?, OWNERS_AFTER);

    // The image line's comment is still the entry's trailing comment.
    let image = deployment
        .get_path(&path!["spec", "template", "spec", "containers", 0, "image"])
        .ok_or("no image")?;
    let image_key = image.key().ok_or("image has no key")?;
    let trailing = image_key.comments()?;
    assert_eq!(
        trailing.trailing().first().map(|c| c.raw()),
        Some("# pinned by release tooling")
    );
    Ok(stream)
}

/// The edited text read by serde means what the edited `Stream`
/// means, document by document; and a typed read agrees.
fn check_meaning(stream: &Stream) -> Result<()> {
    let text = stream.to_string();
    let from_serde = Deserializer::from_str(&text)
        .map(Value::deserialize)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let from_document = stream
        .documents()
        .map(|d| d.to_value())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(from_serde, from_document);

    let deployment: Deployment = stream.document(0).ok_or("no Deployment")?.deserialize()?;
    let web = deployment
        .spec
        .template
        .spec
        .containers
        .first()
        .ok_or("no container")?;
    assert_eq!(web.image, "registry.example.com/web:1.5.0");
    assert_eq!(web.image_pull_policy.as_deref(), Some("IfNotPresent"));
    let ports: Vec<_> = web
        .ports
        .iter()
        .map(|p| (p.name.as_str(), p.container_port))
        .collect();
    assert_eq!(ports, [("metrics", 9090)]);
    let env: Vec<_> = web
        .env
        .iter()
        .map(|e| (e.name.as_str(), e.value.as_str()))
        .collect();
    assert_eq!(env, [("LOG_LEVEL", "info"), ("FEATURE_SEARCH", "on")]);

    // A per-node typed read: just the container, O(that node), meaning
    // what the whole-document read means there.
    let deployment = stream.document(0).ok_or("no Deployment")?;
    let node = deployment
        .get_path(&path!["spec", "template", "spec", "containers", 0])
        .ok_or("no container")?;
    let container: Container = node.deserialize()?;
    assert_eq!(container.image, web.image);
    assert_eq!(container.image_pull_policy, web.image_pull_policy);
    // The same container moved out as a `Value` and read again
    // (`from_value`, serde_yaml's shape).
    let again: Container = oneyaml::from_value(node.deserialize::<Value>()?)?;
    let ports: Vec<_> = again
        .ports
        .iter()
        .map(|p| (p.name.as_str(), p.container_port))
        .collect();
    assert_eq!(ports, [("metrics", 9090)]);
    assert_eq!(again.env.len(), container.env.len());
    Ok(())
}

/// The Service, annotated through its own [`DocumentMut`]: document-level
/// edits on the guard, node edits on the handles it gives (each changes
/// the stream). On a mapping value's handle, comment edits act on its
/// entry (the key owns the entry's comments).
fn annotate_service(stream: &mut Stream) -> Result<()> {
    let mut svc: DocumentMut<'_> = stream.document_mut(1).ok_or("no Service")?;
    svc.set_header(["Service: the stable address in front of the Deployment."])?;
    svc.get_path_mut(&path!["spec", "ports", 0, "port"])?
        .set_trailing(Some("what the ingress calls"))?;
    svc.get_path_mut(&path!["spec", "ports"])?
        .push_footer("- port: 443 once TLS moves here")?;
    svc.get_mut("spec")?.set_blank_lines_before(1)?;

    // The Service's own text is its region of the file: its header (before
    // `---`) and its comments included. Parsed on its own, it has the same
    // comments with the same owners.
    let service = stream.document(1).ok_or("no Service")?;
    let text = service.to_string();
    assert!(text.starts_with("# Service: the stable address"));
    assert!(stream.to_string().ends_with(&text));
    let alone = Document::parse(text.clone())?;
    assert_eq!(alone.to_string(), text);
    assert_eq!(common::owners(&alone)?, common::owners(&service)?);

    // Spans are offsets in the whole file; `span().start` shifts them into
    // the document's own text.
    let base = service.span().start;
    for e in service.explain_comments()? {
        let (from, to) = (
            e.span().start.saturating_sub(base),
            e.span().end.saturating_sub(base),
        );
        assert_eq!(text.get(from..to), Some(e.text()));
        assert_eq!(e.blank_lines_before(), 0);
    }
    let target = service
        .get_path(&path!["spec", "ports", 0, "targetPort"])
        .and_then(|n| n.key())
        .ok_or("targetPort has no key")?;
    let note = service.comments(target.id())?;
    let note = note.trailing().first().ok_or("no note")?;
    let why = service.explain_comment(note.id())?;
    assert_eq!(why.span(), note.span());
    assert_eq!(why.owner_span(), Some(target.span()));
    assert_eq!(why.rule(), CommentRule::Trailing);
    Ok(())
}

/// What-ifs on a copy of the stream (O(1) until edited; the stream itself
/// never changes): re-owning a comment for this reader only, and moving a
/// port between documents.
fn what_ifs(stream: &Stream) -> Result<()> {
    let mut preview = stream.clone();
    let mut deployment = preview.document_mut(0).ok_or("no Deployment")?;
    let ports_path = path!["spec", "template", "spec", "containers", 0, "ports"];
    let ports = deployment.id_of(&ports_path).ok_or("no ports")?;
    let tls = deployment
        .comment_ids()
        .into_iter()
        .find(|&id| {
            deployment
                .explain_comment(id)
                .is_ok_and(|e| e.text().starts_with("# TLS"))
        })
        .ok_or("no TLS note")?;
    // This reader holds the TLS note to be about the list, not the
    // `metrics` port below it: an explicit owner, for reads only.
    deployment.attach_comment(tls, CommentOwner::Footer(ports))?;
    let footer = deployment.comments(ports)?;
    assert!(footer.footer().first().is_some_and(|c| c.is_explicit()));
    assert_eq!(
        deployment.explain_comment(tls)?.rule(),
        CommentRule::Explicit
    );
    assert_eq!(
        deployment.detach_comment(tls)?,
        Some(CommentOwner::Footer(ports))
    );
    assert_eq!(
        deployment.explain_comment(tls)?.rule(),
        CommentRule::Leading
    );

    // A port removed from the Deployment cannot go into the Service
    // (anchors are per document): the refusal hands it back.
    let mut metrics_path = ports_path.clone();
    metrics_path.push_index(0);
    let metrics = deployment.remove_path(&metrics_path)?;
    let mut service = preview.document_mut(1).ok_or("no Service")?;
    let err = service
        .get_path_mut(&path!["spec", "ports"])?
        .insert_removed(metrics)
        .err()
        .ok_or("a port moved between documents")?;
    assert_eq!(err.error(), &EditError::WrongDocument);
    assert_eq!(err.removed().comments().len(), 3);
    let (why, metrics) = err.into_parts();
    assert_eq!(why, EditError::WrongDocument);
    // Nor back: its list, left without ports, became `[]`, and block
    // content with comments cannot go into a flow collection yet (limitation
    // P1). Where only the reason matters, `ReinsertError::into_error` gives
    // the plain `EditError` (for `?`).
    let mut deployment = preview.document_mut(0).ok_or("no Deployment")?;
    let back = deployment
        .get_path_mut(&ports_path)?
        .insert_removed(metrics)
        .map(|item| item.id())
        .map_err(ReinsertError::into_error);
    assert!(matches!(back, Err(EditError::Unsupported { .. })));
    // The preview is dropped; the stream never changed.
    assert_ne!(preview.to_string(), stream.to_string());
    Ok(())
}

fn run() -> Result<String> {
    let mut stream = edit()?;
    annotate_service(&mut stream)?;
    what_ifs(&stream)?;
    assert_eq!(stream.to_string(), EDITED);
    check_meaning(&stream)?;
    // The untouched input round-trips byte for byte.
    assert_eq!(
        Stream::parse(MANIFEST)?.to_string().as_bytes(),
        MANIFEST.as_bytes()
    );
    Ok(stream.to_string())
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> Result<()> {
    print!("{}", run()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn k8s_manifest() -> super::Result<()> {
        super::run().map(|_| ())
    }
}
