//! Local Railway TUI attached to the platform-owned daemon through its public gate.
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use is_terminal::IsTerminal;
use reqwest_websocket::{Message, RequestBuilderExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ClientAction, LaunchArgs, Progress, SessionStyle, saved_config::SavedConfig};
use crate::{
    client::{GQLClient, post_graphql},
    commands::cloud_agent::access,
    config::Configs,
    controllers::cloud_agent as ca,
    gql::{mutations, queries},
};

mod bridge;

// Known attach-capable public release. Server lifecycle/version is owned by the VM image.
const VERSION: &str = "0.1.15";

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Connection {
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

struct ProgressReporter;
impl Progress for ProgressReporter {
    fn step(&self, text: &str) {
        eprintln!("{text}");
    }
    fn note(&self, text: &str) {
        eprintln!("{text}");
    }
    fn finish(&self) {}
}

pub(super) async fn command(mut args: LaunchArgs, action: ClientAction) -> Result<()> {
    let interactive =
        !args.connection_json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if args.code_endpoint || args.code_port.is_some() {
        bail!(
            "Railway uses its existing agent endpoint; --code-endpoint and --code-port are unnecessary"
        );
    }
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    access::ensure_enabled(&client, &configs).await?;
    // Install before creating a VM, so an unavailable platform/release costs no VM.
    let binary = if interactive {
        Some(ensure_client(VERSION).await?)
    } else {
        None
    };
    let (saved, directory) = match action {
        ClientAction::Local => {
            super::client::pin_agent(&mut args).await?;
            let directory = args.remote_dir.take().unwrap_or_else(|| "/app".into());
            args.app_mode = true;
            let prepared =
                super::prepare(&args, &ProgressReporter, SessionStyle::FullTerminal).await?;
            (SavedConfig::from_prepared(&prepared)?, directory)
        }
        ClientAction::Connect(selector) => {
            let mut configs = configs;
            let scope = if args.project.is_some() || args.environment.is_some() {
                Some(
                    super::resolve_project_and_env(
                        &mut configs,
                        &client,
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
                ca::resolve(&configs, &client, selector.as_deref(), scope.as_deref()).await?;
            if !agent.status.is_live() {
                bail!("{} is {}", agent.name, agent.status.label());
            }
            if agent.status == ca::Status::Sleeping {
                eprintln!("Waking {}…", agent.name);
                ca::wake(&client, &configs.get_backboard(), &agent.id).await?;
            }
            let directory =
                super::saved_config::railway_connection(&agent.id, &agent.environment_id)
                    .map(|c| c.directory)
                    .unwrap_or_else(|| "/app".into());
            (
                SavedConfig::new(
                    &agent.id,
                    &agent.name,
                    &agent.environment_id,
                    "railway",
                    None,
                )?,
                directory,
            )
        }
        _ => bail!("Unsupported Railway local-client action"),
    };
    let configs = Configs::new()?;
    let backboard = configs.get_backboard();
    eprintln!(
        "Connecting to Railway's agent endpoint on {}…",
        saved.agent_name
    );
    let url = endpoint(&client, &backboard, &saved.agent_id, &saved.environment_id).await?;
    let mut target = Target {
        client,
        backboard,
        agent_id: saved.agent_id.clone(),
        environment_id: saved.environment_id.clone(),
        connection: Connection {
            url,
            directory,
            client_version: VERSION.into(),
        },
    };
    target.connection.directory = verify_connection(&target).await?;
    let saved = saved.with_railway(&target.connection);
    saved.save()?;
    if args.connection_json {
        println!(
            "{}",
            serde_json::json!({"schemaVersion": 1,
            "agent": {"id": saved.agent_id, "name": saved.agent_name, "environmentId": saved.environment_id},
            "connection": {"transport": "wss", "url": target.connection.url,
                "directory": target.connection.directory, "clientVersion": VERSION,
                "authentication": "cloudAgentHarnessToken", "tokenLifetimeSeconds": 300}})
        );
        return Ok(());
    }
    saved.show()?;
    if let Some(binary) = binary {
        // A local protocol bridge adds a freshly minted gate token on every dial.
        // It carries no SSH traffic and never runs a local agent daemon.
        let bridge = bridge::Bridge::start(target)?;
        eprintln!("Launching local Railway TUI…");
        let status = tokio::process::Command::new(binary)
            .args(["--attach", &bridge.url])
            .kill_on_drop(true)
            .status()
            .await
            .context("Launching the local Railway TUI; the remote daemon is still running")?;
        if !status.success() {
            bail!("Railway TUI exited with {status}; rerun connect to reattach");
        }
    }
    Ok(())
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
fn validate_version(version: &str) -> Result<()> {
    if !regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$")?.is_match(version) {
        bail!("Unsupported Railway agent version");
    }
    Ok(())
}

fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        _ => bail!("Railway TUI releases support macOS and Linux; use WSL or --railway remote"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => bail!("Railway TUI releases require amd64 or arm64"),
    };
    Ok(format!("{os}-{arch}"))
}

async fn compatible(binary: &std::path::Path, version: &str) -> bool {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--version").kill_on_drop(true);
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await else {
        return false;
    };
    output.status.success()
        && String::from_utf8_lossy(&output.stdout).trim() == format!("railway-agent-tui {version}")
}

async fn ensure_client(version: &str) -> Result<PathBuf> {
    validate_version(version)?;
    let platform = platform()?;
    let root = dirs::home_dir()
        .context("Unable to get home directory")?
        .join(".railway/runtimes/railway-tui")
        .join(version);
    let binary = root.join("railway-agent-tui");
    if compatible(&binary, version).await {
        return Ok(binary);
    }
    if let Ok(candidate) = which::which("railway-agent-tui") {
        if compatible(&candidate, version).await {
            return Ok(candidate);
        }
    }
    eprintln!("Installing Railway TUI {version} for {platform}…");
    let tag = format!("v{version}");
    let name = format!("railway-agent-tui-{tag}-{platform}.tar.gz");
    let client = reqwest::Client::builder()
        .user_agent("railway-cli")
        .timeout(Duration::from_secs(120))
        .build()?;
    let release: serde_json::Value = client
        .get(format!(
            "https://api.github.com/repos/railwayapp/agent-releases/releases/tags/{tag}"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let asset = release["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"] == name))
        .with_context(|| {
            format!("Railway {tag} has no TUI release for {platform}; use --railway remote")
        })?;
    let digest = asset["digest"]
        .as_str()
        .context("Release asset is missing its checksum")?;
    let data = client
        .get(format!(
            "https://github.com/railwayapp/agent-releases/releases/download/{tag}/{name}"
        ))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if format!("sha256:{:x}", Sha256::digest(&data)) != digest {
        bail!("Railway TUI download checksum mismatch");
    }
    std::fs::create_dir_all(&root)?;
    // Extract only the two regular executable files into a private temporary directory.
    let temporary = tempfile::tempdir_in(&root)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(data.as_ref()));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        if ["railway-agent-tui", "railway-agent"]
            .iter()
            .any(|expected| name == std::path::Path::new(expected))
        {
            if !entry.header().entry_type().is_file() {
                bail!("Invalid Railway release archive");
            }
            entry.unpack(temporary.path().join(name))?;
        }
    }
    if !compatible(&temporary.path().join("railway-agent-tui"), version).await
        || !temporary.path().join("railway-agent").is_file()
    {
        bail!("Downloaded Railway TUI does not match its server");
    }
    for name in ["railway-agent", "railway-agent-tui"] {
        std::fs::rename(temporary.path().join(name), root.join(name))?;
    }
    Ok(binary)
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
        for bad in ["../1.2.3", "latest", "1.2.3/elsewhere", "1.2.3 --flag"] {
            assert!(validate_version(bad).is_err());
        }
        assert!(validate_version(VERSION).is_ok());
    }

    #[tokio::test]
    #[ignore = "downloads the official Railway TUI release"]
    async fn official_client_installs_and_is_reused() {
        let binary = ensure_client(VERSION).await.unwrap();
        assert!(compatible(&binary, VERSION).await);
        assert_eq!(binary, ensure_client(VERSION).await.unwrap());
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
                client_version: VERSION.into(),
            },
        };
        assert_eq!(verify_connection(&target).await.unwrap(), "/app");
    }
}
