//! Shared by launch, the flat commands, and the TUI's save form.
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{client::post_graphql, config::Configs, gql::bootstraps as gql};

/// Bootstrap fields are currently exposed only on Backboard's internal schema.
pub fn internal_url(url: &str) -> String {
    match url.strip_suffix("/graphql/v2") {
        Some(base) => format!("{base}/graphql/internal"),
        None => url.to_owned(),
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub id: String,
    pub name: String,
    pub environment_id: String,
    pub status: String,
    pub failure_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub is_default: bool,
    pub source_agent_id: Option<String>,
    pub checkpoint_id: Option<String>,
}

macro_rules! from_fragment {
    ($($module:ident),+) => {$(
        impl From<gql::$module::BootstrapFields> for Bootstrap {
            fn from(b: gql::$module::BootstrapFields) -> Self {
                Self {
                    id: b.id, name: b.name, environment_id: b.environment_id,
                    status: match b.status {
                        gql::$module::AgentBootstrapStatus::READY => "READY".into(),
                        gql::$module::AgentBootstrapStatus::SAVING => "SAVING".into(),
                        gql::$module::AgentBootstrapStatus::DEGRADED => "DEGRADED".into(),
                        gql::$module::AgentBootstrapStatus::Other(s) => s,
                    },
                    failure_reason: b.failure_reason, updated_at: b.updated_at,
                    is_default: false,
                    source_agent_id: b.active_version.as_ref().map(|v| v.source_cloud_agent_id.clone()),
                    checkpoint_id: b.active_version.map(|v| v.checkpoint.id),
                }
            }
        }
    )+};
}
from_fragment!(agent_bootstraps, agent_bootstrap, agent_bootstrap_save);

pub async fn list(
    configs: &Configs,
    client: &reqwest::Client,
    url: &str,
    environment_id: &str,
) -> Result<Vec<Bootstrap>> {
    let data = post_graphql::<gql::AgentBootstraps, _>(
        client,
        internal_url(url),
        gql::agent_bootstraps::Variables {
            environment_id: environment_id.into(),
        },
    )
    .await?;
    let default = configs.get_agent_bootstrap_default(environment_id);
    Ok(data
        .agent_bootstraps
        .into_iter()
        .map(|b| {
            let mut b = Bootstrap::from(b);
            b.is_default = default == Some(b.id.as_str());
            b
        })
        .collect())
}

pub async fn get(client: &reqwest::Client, url: &str, id: &str) -> Result<Bootstrap> {
    Ok(post_graphql::<gql::AgentBootstrap, _>(
        client,
        internal_url(url),
        gql::agent_bootstrap::Variables { id: id.into() },
    )
    .await?
    .agent_bootstrap
    .into())
}

pub fn select(bootstraps: Vec<Bootstrap>, name: &str) -> Result<Bootstrap> {
    bootstraps
        .into_iter()
        .find(|b| b.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No bootstrap named '{name}' in this environment. Run `railway ca bootstrap list`."
            )
        })
}

impl Bootstrap {
    pub fn require_ready(&self) -> Result<()> {
        if self.status != "READY" {
            bail!(
                "Bootstrap '{}' is {}{}. Save it again or use --no-bootstrap for a clean VM.",
                self.name,
                self.status,
                self.failure_reason
                    .as_ref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            );
        }
        Ok(())
    }
}

/// Called only when creating: reconnecting must never reapply a bootstrap.
pub async fn resolve_for_create(
    configs: &Configs,
    client: &reqwest::Client,
    url: &str,
    environment_id: &str,
    name: Option<&str>,
    no_bootstrap: bool,
) -> Result<Option<Bootstrap>> {
    if no_bootstrap {
        return Ok(None);
    }
    let bootstrap = if let Some(name) = name {
        Some(select(
            list(configs, client, url, environment_id).await?,
            name,
        )?)
    } else {
        match configs.get_agent_bootstrap_default(environment_id) {
            None => None,
            Some(id) => Some(list(configs, client, url, environment_id).await?
                .into_iter().find(|b| b.id == id).ok_or_else(|| anyhow::anyhow!(
                    "Your local default bootstrap is no longer available. Select another with `railway ca bootstrap default <name>` or use --no-bootstrap."
                ))?),
        }
    };
    if let Some(b) = &bootstrap {
        b.require_ready()?;
    }
    Ok(bootstrap)
}

pub async fn save(
    client: &reqwest::Client,
    url: &str,
    agent_id: &str,
    name: &str,
    existing_id: Option<String>,
    variables: Option<serde_json::Value>,
) -> Result<Bootstrap> {
    Ok(post_graphql::<gql::AgentBootstrapSave, _>(
        client,
        internal_url(url),
        gql::agent_bootstrap_save::Variables {
            input: gql::agent_bootstrap_save::AgentBootstrapSaveInput {
                cloud_agent_id: Some(agent_id.into()),
                id: existing_id,
                name: Some(name.into()),
                manifest: None,
                variables,
            },
        },
    )
    .await?
    .agent_bootstrap_save
    .into())
}

/// Capture completion is separate from accepting the save request. Never promote
/// a failed or still-saving capture, and leave the existing default untouched.
pub async fn wait_ready(
    client: &reqwest::Client,
    url: &str,
    mut b: Bootstrap,
) -> Result<Bootstrap> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    while b.status == "SAVING" {
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "Bootstrap '{}' is still saving. Check `railway ca bootstrap list`; the default has not changed.",
                b.name
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        b = get(client, url, &b.id).await?;
    }
    if b.status != "READY" {
        bail!(
            "Bootstrap '{}' is not usable: disk capture failed. Default unchanged. Retry from this VM with the same name.\n{}",
            b.name,
            b.failure_reason.as_deref().unwrap_or("Capture failed")
        );
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    fn row(id: &str, name: &str, status: &str) -> serde_json::Value {
        json!({"id": id, "name": name, "environmentId": "env", "status": status,
            "failureReason": null, "updatedAt": "2026-09-11T00:00:00Z"})
    }

    #[tokio::test]
    async fn bootstrap_local_default_survives_rename_and_explicit_override() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        configs
            .set_agent_bootstrap_default("env", "saved-id", false)
            .await
            .unwrap();
        let client = reqwest::Client::new();
        server.stub(
            "AgentBootstraps",
            json!({"agentBootstraps": [
                row("saved-id", "renamed", "READY"), row("other", "other", "READY")
            ]}),
        );
        let b = resolve_for_create(&configs, &client, &server.url(), "env", None, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b.name, "renamed");
        assert!(b.is_default);
        let b = resolve_for_create(
            &configs,
            &client,
            &server.url(),
            "env",
            Some("other"),
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(b.id, "other");
        assert_eq!(configs.get_agent_bootstrap_default("env"), Some("saved-id"));
        let count = server.requests().len();
        for (env, clean) in [("env", true), ("another-project-env", false)] {
            assert!(
                resolve_for_create(&configs, &client, &server.url(), env, None, clean)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(server.requests().len(), count);
        assert!(
            resolve_for_create(&configs, &client, &server.url(), "env", Some("typo"), false)
                .await
                .unwrap_err()
                .to_string()
                .contains("No bootstrap named")
        );
    }

    #[tokio::test]
    async fn bootstrap_deleted_or_unready_local_default_errors() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        configs
            .set_agent_bootstrap_default("env", "saved", false)
            .await
            .unwrap();
        let client = reqwest::Client::new();
        for status in ["SAVING", "DEGRADED", "FUTURE_STATUS"] {
            let server = MockBackboard::spawn();
            server.stub(
                "AgentBootstraps",
                json!({"agentBootstraps": [row("saved", "dev", status)]}),
            );
            assert!(
                resolve_for_create(&configs, &client, &server.url(), "env", None, false)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains(status)
            );
        }
        server.stub("AgentBootstraps", json!({"agentBootstraps": []}));
        assert!(
            resolve_for_create(&configs, &client, &server.url(), "env", None, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("no longer available")
        );
    }

    #[tokio::test]
    async fn bootstrap_local_preferences_persist_without_replacing_existing_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut first = Configs::for_test(path.clone());
        let mut second = Configs::for_test(path.clone());
        assert!(
            first
                .set_agent_bootstrap_default("env", "first", true)
                .await
                .unwrap()
        );
        assert!(
            !second
                .set_agent_bootstrap_default("env", "second", true)
                .await
                .unwrap()
        );
        assert!(
            second
                .set_agent_bootstrap_default("other-env", "other", true)
                .await
                .unwrap()
        );
        assert!(
            first
                .set_agent_bootstrap_default("env", "chosen", false)
                .await
                .unwrap()
        );
        // An older TUI/config instance writing unrelated state must not erase
        // a selection made by another process or the TUI save form.
        second.set_code_agent("env", "vm");
        second.write().unwrap();
        let mut reloaded = Configs::for_test(path);
        reloaded.reload().unwrap();
        assert_eq!(reloaded.get_agent_bootstrap_default("env"), Some("chosen"));
        assert_eq!(
            reloaded.get_agent_bootstrap_default("other-env"),
            Some("other")
        );
    }

    #[tokio::test]
    async fn bootstrap_no_default_persists_and_launches_without_a_checkpoint() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        configs
            .set_agent_bootstrap_default("env", "saved", false)
            .await
            .unwrap();
        configs
            .set_agent_bootstrap_default("other-env", "other", false)
            .await
            .unwrap();
        let mut stale = server.configs(&dir);
        configs.clear_agent_bootstrap_default("env").await.unwrap();
        stale.set_code_agent("env", "remembered");
        stale.write().unwrap();
        configs.reload().unwrap();
        assert_eq!(configs.get_agent_bootstrap_default("env"), None);
        assert_eq!(
            configs.get_agent_bootstrap_default("other-env"),
            Some("other")
        );
        let b = resolve_for_create(
            &configs,
            &reqwest::Client::new(),
            &server.url(),
            "env",
            None,
            false,
        )
        .await
        .unwrap();
        assert!(b.is_none());
        assert!(server.variables_for("AgentBootstraps").is_empty());
    }

    #[tokio::test]
    async fn bootstrap_save_preserves_variables_and_polls_capture() {
        let server = MockBackboard::spawn();
        let client = reqwest::Client::new();
        server.stub(
            "AgentBootstrapSave",
            json!({"agentBootstrapSave": row("saved", "dev", "SAVING")}),
        );
        server.stub(
            "AgentBootstrap",
            json!({"agentBootstrap": row("saved", "dev", "READY")}),
        );
        let saved = save(
            &client,
            &server.url(),
            "vm",
            "dev",
            None,
            Some(json!({"MODE": "dev"})),
        )
        .await
        .unwrap();
        let ready = wait_ready(&client, &server.url(), saved).await.unwrap();
        assert_eq!(ready.status, "READY");
        assert_eq!(
            server.variables_for("AgentBootstrapSave")[0]["input"]["variables"]["MODE"],
            "dev"
        );
        assert_eq!(server.requests().len(), 2);
    }
}
