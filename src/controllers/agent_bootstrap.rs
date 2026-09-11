//! Shared by launch, the flat commands, and the TUI's save form.
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{client::post_graphql, gql::bootstraps as gql};

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
                }
            }
        }
    )+};
}
from_fragment!(
    agent_bootstraps,
    agent_bootstrap,
    agent_bootstrap_default,
    agent_bootstrap_save
);

pub async fn list(
    client: &reqwest::Client,
    url: &str,
    environment_id: &str,
) -> Result<Vec<Bootstrap>> {
    let data = post_graphql::<gql::AgentBootstraps, _>(
        client,
        url,
        gql::agent_bootstraps::Variables {
            environment_id: environment_id.into(),
        },
    )
    .await?;
    let default = data.agent_bootstrap_default.map(|b| b.id);
    Ok(data
        .agent_bootstraps
        .into_iter()
        .map(|b| {
            let mut b = Bootstrap::from(b);
            b.is_default = default.as_deref() == Some(&b.id);
            b
        })
        .collect())
}

pub async fn get(client: &reqwest::Client, url: &str, id: &str) -> Result<Bootstrap> {
    Ok(post_graphql::<gql::AgentBootstrap, _>(
        client,
        url,
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
        Some(select(list(client, url, environment_id).await?, name)?)
    } else {
        post_graphql::<gql::AgentBootstrapDefault, _>(
            client,
            url,
            gql::agent_bootstrap_default::Variables {
                environment_id: environment_id.into(),
            },
        )
        .await?
        .agent_bootstrap_default
        .map(Bootstrap::from)
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
        url,
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

pub async fn set_default(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    only_if_unset: bool,
) -> Result<bool> {
    let data = post_graphql::<gql::AgentBootstrapSetDefault, _>(
        client,
        url,
        gql::agent_bootstrap_set_default::Variables {
            id: id.into(),
            only_if_unset: Some(only_if_unset),
        },
    )
    .await?;
    Ok(data.agent_bootstrap_set_default.is_some_and(|b| b.id == id))
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
    b.require_ready()?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    fn row(name: &str, status: &str) -> serde_json::Value {
        json!({"id": format!("bootstrap-{name}"), "name": name, "environmentId": "linked-env",
            "status": status, "failureReason": null, "updatedAt": "2026-09-11T00:00:00Z"})
    }

    #[tokio::test]
    async fn bootstrap_default_override_and_clean_vm() {
        let server = MockBackboard::spawn();
        let client = reqwest::Client::new();
        server.stub(
            "AgentBootstrapDefault",
            json!({"agentBootstrapDefault": row("default", "READY")}),
        );
        server.stub(
            "AgentBootstraps",
            json!({"agentBootstraps": [row("default", "READY"), row("other", "READY")],
            "agentBootstrapDefault": {"id": "bootstrap-default"}}),
        );
        let b = resolve_for_create(&client, &server.url(), "linked-env", None, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b.id, "bootstrap-default");
        assert_eq!(
            server.variables_for("AgentBootstrapDefault")[0]["environmentId"],
            "linked-env"
        );
        let b = resolve_for_create(&client, &server.url(), "linked-env", Some("other"), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b.id, "bootstrap-other");
        let count = server.requests().len();
        assert!(
            resolve_for_create(&client, &server.url(), "linked-env", None, true)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(server.requests().len(), count);
        assert!(
            resolve_for_create(&client, &server.url(), "linked-env", Some("typo"), false)
                .await
                .unwrap_err()
                .to_string()
                .contains("No bootstrap named 'typo'")
        );
    }

    #[tokio::test]
    async fn bootstrap_missing_default_is_clean_but_unavailable_default_errors() {
        let client = reqwest::Client::new();
        for status in ["SAVING", "DEGRADED", "FUTURE_STATUS"] {
            let server = MockBackboard::spawn();
            server.stub(
                "AgentBootstrapDefault",
                json!({"agentBootstrapDefault": row("default", status)}),
            );
            let error = resolve_for_create(&client, &server.url(), "linked-env", None, false)
                .await
                .unwrap_err();
            assert!(error.to_string().contains(status));
        }
        let server = MockBackboard::spawn();
        server.stub(
            "AgentBootstrapDefault",
            json!({"agentBootstrapDefault": null}),
        );
        assert!(
            resolve_for_create(&client, &server.url(), "linked-env", None, false)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn bootstrap_save_preserves_variables_and_polls_before_promotion() {
        let server = MockBackboard::spawn();
        let client = reqwest::Client::new();
        server.stub(
            "AgentBootstrapSave",
            json!({"agentBootstrapSave": row("dev", "SAVING")}),
        );
        server.stub(
            "AgentBootstrap",
            json!({"agentBootstrap": row("dev", "READY")}),
        );
        server.stub(
            "AgentBootstrapSetDefault",
            json!({"agentBootstrapSetDefault": {"id": "bootstrap-dev"}}),
        );
        let b = save(
            &client,
            &server.url(),
            "source-vm",
            "dev",
            None,
            Some(json!({"MODE": "dev"})),
        )
        .await
        .unwrap();
        let b = wait_ready(&client, &server.url(), b).await.unwrap();
        assert!(
            set_default(&client, &server.url(), &b.id, true)
                .await
                .unwrap()
        );
        let input = &server.variables_for("AgentBootstrapSave")[0]["input"];
        assert_eq!(input["cloudAgentId"], "source-vm");
        assert_eq!(input["variables"]["MODE"], "dev");
        assert_eq!(
            server.variables_for("AgentBootstrapSetDefault")[0]["onlyIfUnset"],
            true
        );
        let ops: Vec<_> = server
            .requests()
            .iter()
            .map(|r| r["operationName"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            ops,
            [
                "AgentBootstrapSave",
                "AgentBootstrap",
                "AgentBootstrapSetDefault"
            ]
        );
    }
}
