//! Real-world example: editing a docker-compose file that
//! shares settings through an anchored `x-common` block and merge keys
//! (`<<`), without losing its comments.
//!
//! Merge keys are a YAML 1.1 extension: YAML 1.2 reads `<<` as an ordinary
//! key, so the file is read with `Settings::merge_keys(true)`. The
//! edits:
//!
//! 1. change a shared setting *through the anchor*: the path
//!    `services.web.environment.LOG_LEVEL` goes through `web`'s merge key
//!    into `x-common`, so the one edit changes every service that merges
//!    it (the entry keeps its trailing comment, which is then updated);
//! 2. override a merged setting for one service: `services.web.restart`
//!    is only merged in, so setting it adds an own entry to `web` that
//!    shadows `x-common`'s (merge keys get no special treatment);
//! 3. add a `worker` service with a leading comment that merges
//!    `x-common` like the others (`insert_merge` writes the merge key
//!    `<<: *common`; `insert_alias("<<", ..)` would ask for the *string*
//!    key `'<<'`, which merges nothing);
//! 4. add a `cron` service that shares only `x-common`'s logging block,
//!    through a new anchor and an alias;
//! 5. extend the document header.
//!
//! Effective values (what the services really get, merges applied) are
//! read three ways: `Document` navigation, a typed `deserialize`, and
//! serde's `from_str_with` — the same answer each time.
//!
//! Run with `cargo run -p oneyaml --example compose_merge`; `cargo test -p
//! oneyaml` runs it as a test.

mod common;

use oneyaml::{Document, Settings, Value, path};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const COMPOSE: &str = "\
# Local development stack.

x-common: &common
  restart: unless-stopped
  environment:
    TZ: UTC
    LOG_LEVEL: info   # raise to debug when chasing a bug
  logging:
    driver: json-file

services:
  web:
    <<: *common
    image: nginx:1.27
    ports:
      - \"8080:80\"
  api:
    <<: *common
    image: example/api:2.3
    restart: always   # overrides x-common
";

const EDITED: &str = "\
# Local development stack.
# Start it with: docker compose up

x-common: &common
  restart: unless-stopped
  environment:
    TZ: UTC
    LOG_LEVEL: debug   # back to info before merging
  logging: &logging
    driver: json-file

services:
  web:
    <<: *common
    image: nginx:1.27
    ports:
      - \"8080:80\"
    restart: 'no'
  api:
    <<: *common
    image: example/api:2.3
    restart: always   # overrides x-common
  # Background jobs: the api image, run as a queue worker.
  worker:
    <<: *common
    image: example/api:2.3
    command:
      - worker
      - '--queue'
      - default
  cron:
    image: example/cron:1.0
    logging: *logging
";

const OWNERS_BEFORE: &[&str] = &[
    "# Local development stack. -> document header (rule 2)",
    "# raise to debug when chasing a bug -> trailing of `LOG_LEVEL` (rule 5)",
    "# overrides x-common -> trailing of `restart` (rule 5)",
];
const OWNERS_AFTER: &[&str] = &[
    "# Local development stack. -> document header (rule 2)",
    "# Start it with: docker compose up -> document header (rule 2)",
    "# back to info before merging -> trailing of `LOG_LEVEL` (rule 5)",
    "# overrides x-common -> trailing of `restart` (rule 5)",
    "# Background jobs: the api image, run as a queue worker. -> leading of `worker` (rule 4)",
];

/// A new service, as the edit API serializes it (`--queue` is quoted: a
/// plain scalar cannot start with `-` followed by one).
#[derive(Serialize)]
struct NewService {
    image: &'static str,
    command: [&'static str; 3],
}

/// A service that takes only the shared logging block.
#[derive(Serialize)]
struct CronService {
    image: &'static str,
}

/// What a service effectively gets, merges applied.
#[derive(Deserialize, Debug, PartialEq)]
struct Service {
    image: String,
    restart: Option<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    logging: Option<Logging>,
}
#[derive(Deserialize, Debug, PartialEq)]
struct Logging {
    driver: String,
}
#[derive(Deserialize)]
struct Compose {
    services: BTreeMap<String, Service>,
}

fn edit(settings: &Settings) -> Result<Document> {
    let mut doc = Document::parse_with(COMPOSE, settings)?;
    assert_eq!(common::owners(&doc)?, OWNERS_BEFORE);

    // Navigation follows merge keys: `web` has no `restart` of its own.
    let restart = doc
        .get_path(&path!["services", "web", "restart"])
        .ok_or("no restart")?;
    assert_eq!(restart.text(), "unless-stopped");
    let common = doc.id_of(&path!["x-common"]).ok_or("no x-common")?;
    assert_eq!(doc.aliases_of(common)?.len(), 2);

    // 1. One edit through the anchor changes every service that merges it.
    doc.set_path(
        &path!["services", "web", "environment", "LOG_LEVEL"],
        "debug",
    )?;
    // The entry kept its note; it is stale now, so replace it (an entry's
    // comments belong to its key).
    doc.get_path_mut(&path!["x-common", "environment", "LOG_LEVEL"])?
        .set_trailing(Some("back to info before merging"))?;

    // 2. An override for `web` alone: a new own entry shadows the merge.
    //    `no` is a boolean to YAML 1.1 readers, so it is written `'no'`.
    doc.set_path(&path!["services", "web", "restart"], "no")?;

    // 3. A new service that merges `x-common`, like `web` and `api`: the
    //    merge key goes first, as they write it.
    let mut services = doc.get_mut("services")?;
    let mut worker = services.insert(
        "worker",
        &NewService {
            image: "example/api:2.3",
            command: ["worker", "--queue", "default"],
        },
    )?;
    worker.insert_merge(&[common])?;
    // A second merge key is refused, with the path of the clash; a refused
    // edit changes nothing.
    let before = worker.document().to_string();
    let err = worker
        .insert_merge(&[common])
        .err()
        .ok_or("a second merge key was accepted")?;
    assert!(matches!(err, oneyaml::EditError::KeyExists { .. }));
    assert_eq!(err.at(), Some(&path!["<<"]));
    assert_eq!(worker.document().to_string(), before);
    worker.set_leading(["Background jobs: the api image, run as a queue worker."])?;
    assert_eq!(doc.aliases_of(common)?.len(), 3);

    // 4. A service that shares only the logging block: a new anchor
    //    (`set_anchor`) and an alias to it (`insert_alias`).
    let logging = doc
        .get_path_mut(&path!["x-common", "logging"])?
        .set_anchor(Some("logging"))?
        .id();
    doc.get_mut("services")?
        .insert(
            "cron",
            &CronService {
                image: "example/cron:1.0",
            },
        )?
        .insert_alias("logging", logging)?;
    assert_eq!(doc.aliases_of(logging)?.len(), 1);

    // 5. One more header line (the header is replaced as a whole).
    let header: Vec<String> = doc.header()?.iter().map(|c| c.text().to_owned()).collect();
    assert_eq!(header, ["Local development stack."]);
    doc.set_header([
        "Local development stack.",
        "Start it with: docker compose up",
    ])?;

    assert_eq!(common::owners(&doc)?, OWNERS_AFTER);
    Ok(doc)
}

fn run() -> Result<String> {
    let settings = Settings::new().merge_keys(true);
    // The untouched input round-trips byte for byte.
    assert_eq!(
        Document::parse_with(COMPOSE, &settings)?
            .to_string()
            .as_bytes(),
        COMPOSE.as_bytes()
    );

    let doc = edit(&settings)?;
    let text = doc.to_string();
    assert_eq!(text, EDITED);

    // Serde, under the same settings, reads the edited text as the
    // `Document` means it.
    let from_serde: Value = oneyaml::from_str_with(&text, &settings)?;
    assert_eq!(from_serde, doc.to_value()?);

    // The effective services, merges applied.
    let compose: Compose = doc.deserialize()?;
    let typed: Compose = oneyaml::from_str_with(&text, &settings)?;
    assert_eq!(compose.services, typed.services);
    let service = |name: &str| compose.services.get(name).ok_or("no such service");
    let (web, api, worker) = (service("web")?, service("api")?, service("worker")?);
    let cron = service("cron")?;
    assert_eq!(web.restart.as_deref(), Some("no"));
    assert_eq!(api.restart.as_deref(), Some("always"));
    assert_eq!(worker.restart.as_deref(), Some("unless-stopped"));
    assert_eq!(cron.restart, None);
    assert!(cron.environment.is_empty());
    for s in [web, api, worker] {
        assert_eq!(
            s.environment.get("LOG_LEVEL").map(String::as_str),
            Some("debug")
        );
        assert_eq!(s.logging, cron.logging);
    }
    assert_eq!(
        cron.logging,
        Some(Logging {
            driver: "json-file".to_owned()
        })
    );

    // Without merge keys (plain YAML 1.2), `<<` is an ordinary key whose
    // value is `x-common`, and nothing is merged.
    let plain: Value = oneyaml::from_str(&text)?;
    let merge = plain
        .get("services")
        .and_then(|s| s.get("api"))
        .and_then(|api| api.get("<<"));
    assert!(merge.is_some_and(Value::is_mapping));
    Ok(text)
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> Result<()> {
    print!("{}", run()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn compose_merge() -> super::Result<()> {
        super::run().map(|_| ())
    }
}
