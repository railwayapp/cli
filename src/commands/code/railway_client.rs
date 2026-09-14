//! Local Railway TUI attached to the platform-owned daemon through its public gate.
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest_websocket::{Message, RequestBuilderExt};
use serde::{Deserialize, Serialize};

use super::{ClientAction, LaunchArgs, Progress, SessionStyle, saved_config::SavedConfig};
use crate::{
    client::{GQLClient, post_graphql},
    commands::cloud_agent::access,
    config::Configs,
    controllers::cloud_agent as ca,
    gql::{mutations, queries},
};

pub(crate) mod bridge;
pub(crate) mod installer;
mod sessions;
pub(crate) use sessions::ClientConnection;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Connection {
    pub url: String,
    pub directory: String,
    pub client_version: String,
}

#[derive(Clone)]
struct Target {
    client: reqwest::Client,
    backboard: String,
    agent_id: String,
    environment_id: String,
    connection: Connection,
}

pub(super) async fn command(mut args: LaunchArgs, action: ClientAction) -> Result<()> {
    use super::client::{self, ConnectionProgress};
    let json = args.connection_json;
    let interactive = !json && client::interactive();
    validate_args(&args)?;
    let configs = Configs::new()?;
    let gql = GQLClient::new_authorized(&configs)?;
    access::ensure_enabled(&gql, &configs).await?;
    let progress = ConnectionProgress::new(json);
    let result: Result<_> = async {
        // Check before provisioning, including JSON and reconnect invocations.
        let installed = installer::ensure_client(&progress).await?;
        let saved = match action {
            ClientAction::Local => {
                client::pin_agent(&mut args).await?;
                args.app_mode = true;
                let prepared = super::prepare(&args, &progress, SessionStyle::FullTerminal).await?;
                SavedConfig::from_prepared(&prepared)?
            }
            ClientAction::Connect(selector) => {
                let mut configs = configs;
                let scope = if args.project.is_some() || args.environment.is_some() {
                    Some(
                        super::resolve_project_and_env(
                            &mut configs,
                            &gql,
                            args.project.take(),
                            args.environment.take(),
                        )
                        .await?
                        .1,
                    )
                } else {
                    None
                };
                let (agent, _) =
                    ca::resolve(&configs, &gql, selector.as_deref(), scope.as_deref()).await?;
                if !agent.status.is_live() {
                    bail!("{} is {}", agent.name, agent.status.label());
                }
                if agent.status == ca::Status::Sleeping {
                    progress.step(&format!("Waking {}", agent.name));
                    ca::wake(&gql, &configs.get_backboard(), &agent.id).await?;
                }
                SavedConfig::new(
                    &agent.id,
                    &agent.name,
                    &agent.environment_id,
                    "railway",
                    None,
                )?
            }
            _ => bail!("Unsupported Railway local-client action"),
        };
        let connection = prepare_connection(
            &saved,
            args.remote_dir.as_deref(),
            &installed.version,
            &progress,
        )
        .await?;
        Ok((
            installed,
            saved.with_railway(&connection.connection),
            connection,
        ))
    }
    .await;
    progress.finish();
    let (installed, saved, connection) = result?;
    let persisted = saved.save();
    let connection = crate::commands::cloud_agent::client_sessions::Connection::Railway(connection);
    if json {
        if let Err(error) = &persisted {
            eprintln!("Could not save connection details for railway code get-config: {error:#}");
        }
        return client::print_connection_json(
            &connection,
            &saved.agent_id,
            &saved.agent_name,
            &saved.environment_id,
        );
    }
    client::clear_setup_output();
    connection.show(&saved, &persisted)?;
    if interactive {
        client::launch_binary(
            &connection,
            &saved,
            &persisted,
            None,
            installed.binary,
            "Railway",
        )
        .await?;
    }
    Ok(())
}

fn validate_args(args: &LaunchArgs) -> Result<()> {
    if args.code_endpoint || args.code_port.is_some() {
        bail!(
            "Railway uses its existing agent endpoint; --code-endpoint and --code-port are unnecessary"
        );
    }
    Ok(())
}

async fn prepare_connection(
    saved: &SavedConfig,
    directory: Option<&str>,
    version: &str,
    progress: &dyn Progress,
) -> Result<ClientConnection> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();
    progress.step("Checking Railway's public endpoint");
    let url = endpoint(&client, &backboard, &saved.agent_id, &saved.environment_id).await?;
    let directory = directory
        .map(str::to_owned)
        .or_else(|| {
            super::saved_config::railway_connection(&saved.agent_id, &saved.environment_id)
                .map(|c| c.directory)
        })
        .unwrap_or_else(|| "/app".into());
    let mut target = Target {
        client,
        backboard,
        agent_id: saved.agent_id.clone(),
        environment_id: saved.environment_id.clone(),
        connection: Connection {
            url,
            directory,
            client_version: version.into(),
        },
    };
    target.connection.directory = verify_connection(&target).await?;
    Ok(ClientConnection::new(
        target.connection,
        &saved.agent_id,
        &saved.environment_id,
    ))
}

pub(crate) async fn prepare_pane(
    mut args: LaunchArgs,
    progress: &dyn Progress,
) -> Result<crate::commands::cloud_agent::tui::ClientPane> {
    validate_args(&args)?;
    let installed = installer::ensure_client(progress).await?;
    super::client::pin_agent(&mut args).await?;
    args.app_mode = true;
    let prepared = super::prepare(&args, progress, SessionStyle::Pane).await?;
    let saved = SavedConfig::from_prepared(&prepared)?;
    let connection = prepare_connection(
        &saved,
        args.remote_dir.as_deref(),
        &installed.version,
        progress,
    )
    .await?;
    saved.with_railway(&connection.connection).save()?;
    Ok(crate::commands::cloud_agent::tui::ClientPane {
        agent_id: prepared.agent_id,
        agent_name: prepared.agent_name,
        environment_id: prepared.environment_id,
        binary: installed.binary,
        connection: crate::commands::cloud_agent::client_sessions::Connection::Railway(connection),
        thread: None,
        prompt: args.initial_prompt,
    })
}

pub(crate) async fn reconnect(agent_id: &str, environment_id: &str) -> Result<ClientConnection> {
    let saved = super::saved_config::railway_connection(agent_id, environment_id)
        .context("No saved local Railway connection; run railway code --railway connect first")?;
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let url = endpoint(&client, &configs.get_backboard(), agent_id, environment_id).await?;
    Ok(ClientConnection::new(
        Connection { url, ..saved },
        agent_id,
        environment_id,
    ))
}

async fn endpoint(
    client: &reqwest::Client,
    backboard: &str,
    id: &str,
    environment: &str,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let response = post_graphql::<queries::CloudAgentHarnessEndpoint, _>(
            client,
            backboard,
            queries::cloud_agent_harness_endpoint::Variables {
                id: id.into(),
                environment_id: environment.into(),
            },
        )
        .await?;
        let agent = response
            .cloud_agent
            .context("The cloud agent no longer exists")?;
        if let Some(url) = agent.agent_ws_url {
            validate_url(&url)?;
            return Ok(url);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "This VM has no available Railway agent endpoint. Older VMs may need to be recreated; use --railway remote for a terminal session."
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn validate_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).context("Invalid Railway agent endpoint")?;
    if url.scheme() != "wss"
        || url.host_str().is_none()
        || url.path() != "/agent"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Railway's agent endpoint must be a WSS /agent URL without embedded credentials");
    }
    Ok(url)
}

impl Target {
    async fn open(&self, session_id: Option<&str>) -> Result<reqwest_websocket::WebSocket> {
        let token = post_graphql::<mutations::CloudAgentHarnessToken, _>(
            &self.client,
            &self.backboard,
            mutations::cloud_agent_harness_token::Variables {
                id: self.agent_id.clone(),
                environment_id: self.environment_id.clone(),
            },
        )
        .await?
        .cloud_agent_harness_token;
        let mut url = validate_url(&self.connection.url)?;
        url.query_pairs_mut()
            .append_pair("token", &token)
            .append_pair("cwd", &self.connection.directory);
        if let Some(id) = session_id {
            url.query_pairs_mut().append_pair("session_id", id);
        }
        open_gate(url).await
    }
}

async fn open_gate(url: url::Url) -> Result<reqwest_websocket::WebSocket> {
    // A separate client keeps Railway API authorization headers off the gate.
    // Errors must not retain the URL, which contains a short-lived credential.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    tokio::time::timeout(Duration::from_secs(15), async {
        let response = client
            .get(url)
            .upgrade()
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Railway agent WebSocket request failed"))?;
        if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
            bail!(
                "Railway agent endpoint rejected the WebSocket ({})",
                response.status()
            );
        }
        response
            .into_websocket()
            .await
            .map_err(|_| anyhow::anyhow!("Railway agent WebSocket upgrade failed"))
    })
    .await
    .context("Railway agent connection timed out")?
}

async fn verify_connection(target: &Target) -> Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut socket = loop {
        match target.open(None).await {
            Ok(socket) => break socket,
            Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    };
    let public_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let unauthenticated = public_client
        .get(&target.connection.url)
        .upgrade()
        .send()
        .await?;
    if unauthenticated.status() != reqwest::StatusCode::UNAUTHORIZED {
        bail!("Railway's agent endpoint did not reject an unauthenticated WebSocket");
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        socket
            .send(Message::Text(
                serde_json::json!({"type":"get_state", "id":"railway-cli-verify"}).to_string(),
            ))
            .await?;
        while let Some(frame) = socket.next().await {
            match frame? {
                Message::Text(text) => {
                    let value: serde_json::Value = serde_json::from_str(&text)?;
                    if value["id"] != "railway-cli-verify" {
                        continue;
                    }
                    if value["success"] == false {
                        bail!("Railway daemon rejected get_state");
                    }
                    let directory = value["data"]["cwd"]
                        .as_str()
                        .filter(|cwd| !cwd.is_empty())
                        .context("Railway daemon returned no working directory")?
                        .to_owned();
                    SinkExt::close(&mut socket).await?;
                    return Ok(directory);
                }
                Message::Ping(data) => socket.send(Message::Pong(data)).await?,
                _ => {}
            }
        }
        bail!("Railway daemon disconnected before returning its state")
    })
    .await
    .context("Railway daemon state check timed out")?
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_requires_tls_and_separate_authentication() {
        for bad in [
            "ws://host/agent",
            "wss://host/",
            "wss://user:secret@host/agent",
            "wss://host/agent?token=secret",
            "wss://host/agent#fragment",
        ] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
        assert!(validate_url("wss://agent.example.com/agent").is_ok());
    }

    #[tokio::test]
    #[ignore = "requires an existing live Railway VM in RAILWAY_CLOUD_AGENT_ID and RAILWAY_ENVIRONMENT_ID"]
    async fn live_gate_reports_remote_directory() {
        let configs = Configs::new().unwrap();
        let client = GQLClient::new_authorized(&configs).unwrap();
        let backboard = configs.get_backboard();
        let agent_id = std::env::var("RAILWAY_CLOUD_AGENT_ID").unwrap();
        let environment_id = std::env::var("RAILWAY_ENVIRONMENT_ID").unwrap();
        let url = endpoint(&client, &backboard, &agent_id, &environment_id)
            .await
            .unwrap();
        let target = Target {
            client,
            backboard,
            agent_id,
            environment_id,
            connection: Connection {
                url,
                directory: "/app".into(),
                client_version: "0.1.15".into(),
            },
        };
        assert_eq!(verify_connection(&target).await.unwrap(), "/app");
    }
}
