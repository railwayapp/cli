//! Authenticated Codex App Server on the cloud agent's public WebSocket endpoint.
use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest_websocket::{Message, RequestBuilderExt};
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::commands::{
    code::{ConnectInfo, Prepared},
    ssh::native,
};
use crate::util::shell::shell_join;

pub(crate) mod bridge;
pub(crate) mod local;

const BOOTSTRAP: &str = include_str!("codex.py");
const RESULT_PREFIX: &str = "RAILWAY_CODEX_CONNECTION=";
pub(crate) const TOKEN_ENV: &str = "RAILWAY_CODEX_SERVER_TOKEN";

// No Debug: the connection contains a bearer token.
#[derive(Clone, Deserialize, serde::Serialize)]
pub(crate) struct Connection {
    pub url: String,
    pub token: String,
    pub directory: String,
    pub version: String,
    pub reused: bool,
}

async fn bootstrap(info: &ConnectInfo, request: serde_json::Value) -> Result<String> {
    let mut options = info.relay_opts.clone();
    if let Some(identity) = &info.identity {
        options.extend(["-i".into(), identity.to_string_lossy().into_owned()]);
    }
    options.extend(native::relay_port_args());
    let python = shell_join(&["python3".into(), "-c".into(), BOOTSTRAP.into()]);
    let mut child = tokio::process::Command::new("ssh")
        .args(options)
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20", "--"])
        .arg(native::relay_destination(&info.ssh_target))
        .arg(shell_join(&["bash".into(), "-lc".into(), python]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Failed to start Codex setup over SSH")?;
    let mut stdin = child.stdin.take().context("SSH stdin unavailable")?;
    stdin.write_all(&serde_json::to_vec(&request)?).await?;
    drop(stdin);
    let timeout = if request["action"] == "inspect" {
        15
    } else {
        360
    };
    let output = tokio::time::timeout(Duration::from_secs(timeout), child.wait_with_output())
        .await
        .context("Codex setup timed out; rerun connect to check the server")??;
    if !output.status.success() {
        // stdout may contain credentials even if SSH subsequently failed.
        bail!(
            "Codex setup failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix(RESULT_PREFIX))
        .map(str::to_owned)
        .context("SSH returned no Codex setup result")
}

pub(crate) async fn start_prepared(
    prepared: &Prepared,
    directory: &str,
    token: &str,
) -> Result<Connection> {
    let info = ConnectInfo {
        ssh_target: prepared.ssh_target.clone(),
        identity: prepared.identity.clone(),
        relay_opts: prepared.relay_opts.clone(),
    };
    connect_with(&info, json!({"directory": directory, "token": token})).await
}

pub(crate) async fn inspect(info: &ConnectInfo) -> Result<bool> {
    let result = bootstrap(info, json!({"action": "inspect"})).await?;
    let value: serde_json::Value = serde_json::from_str(&result)?;
    Ok(value["directory"]
        .as_str()
        .is_some_and(|dir| !dir.is_empty()))
}

pub(crate) async fn reconnect(info: &ConnectInfo) -> Result<Connection> {
    connect_with(info, json!({"action": "connect"})).await
}

async fn connect_with(info: &ConnectInfo, request: serde_json::Value) -> Result<Connection> {
    let connection: Connection = serde_json::from_str(&bootstrap(info, request).await?)
        .context("Invalid Codex connection result")?;
    validate_url(&connection.url)?;
    local::validate_version(&connection.version)?;
    verify_connection(&connection).await?;
    trust_directory(&connection, &connection.directory).await?;
    Ok(connection)
}

/// A short-lived authenticated App Server connection. All requests, including
/// notifications that precede their reply, have a bounded lifetime.
pub(super) struct Rpc {
    socket: reqwest_websocket::WebSocket,
    next_id: u64,
}

impl Rpc {
    pub(super) async fn connect(connection: &Connection) -> Result<Self> {
        let mut endpoint = validate_url(&connection.url)?;
        endpoint.set_scheme("https").expect("WSS can use HTTPS");
        Self::open(endpoint.as_str(), &connection.token).await
    }

    async fn open(endpoint: &str, token: &str) -> Result<Self> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?;
            let response = client
                .get(endpoint)
                .bearer_auth(token)
                .upgrade()
                .send()
                .await?;
            response.error_for_status_ref()?;
            let mut rpc = Self {
                socket: response.into_websocket().await?,
                next_id: 0,
            };
            rpc.call(
                "initialize",
                json!({
                    "clientInfo": {"name": "railway_cli", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {"experimentalApi": true}
                }),
            )
            .await?;
            rpc.socket
                .send(Message::Text(json!({"method": "initialized"}).to_string()))
                .await?;
            Ok(rpc)
        })
        .await
        .context("Codex connection timed out")?
    }

    pub(super) async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.next_id += 1;
        let id = self.next_id;
        tokio::time::timeout(Duration::from_secs(10), async {
            self.socket
                .send(Message::Text(
                    json!({"id": id, "method": method, "params": params}).to_string(),
                ))
                .await?;
            while let Some(frame) = self.socket.next().await {
                match frame? {
                    Message::Text(text) => {
                        let value: serde_json::Value = serde_json::from_str(&text)?;
                        if value["id"] != id {
                            continue;
                        }
                        if let Some(error) = value.get("error") {
                            bail!(
                                "Codex {method} failed: {}",
                                error["message"].as_str().unwrap_or("App Server error")
                            );
                        }
                        return value
                            .get("result")
                            .cloned()
                            .context("Codex returned no result");
                    }
                    Message::Ping(data) => self.socket.send(Message::Pong(data)).await?,
                    _ => {}
                }
            }
            bail!("Codex disconnected during {method}")
        })
        .await
        .with_context(|| format!("Codex {method} timed out"))?
    }
}

/// Use the same remote config API as Codex's trust screen. The trust target may
/// be a repository root above cwd; trusting only cwd would leave that prompt up.
pub(crate) async fn trust_directory(connection: &Connection, directory: &str) -> Result<()> {
    let mut rpc = Rpc::connect(connection).await?;
    trust_with(&mut rpc, directory).await
}

async fn trust_with(rpc: &mut Rpc, directory: &str) -> Result<()> {
    let config = rpc
        .call(
            "config/read",
            json!({"cwd": directory, "includeLayers": true}),
        )
        .await?;
    let target = trust_target(&config, directory);
    if config["config"]["projects"][&target]["trust_level"] != "trusted" {
        rpc.call(
            "config/value/write",
            json!({
                "keyPath": format!("projects.{}.trust_level", serde_json::to_string(&target)?),
                "value": "trusted", "mergeStrategy": "replace"
            }),
        )
        .await?;
    }
    Ok(())
}

fn trust_target(config: &serde_json::Value, directory: &str) -> String {
    config["layers"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .filter(|layer| layer["name"]["type"] == "project")
        .find_map(|layer| {
            let reason = layer["disabledReason"].as_str()?;
            reason
                .split_once(", add ")
                .and_then(|(_, rest)| rest.rsplit_once(" as a trusted project in "))
                .map(|(path, _)| path)
                .or_else(|| {
                    layer["name"]["dotCodexFolder"]
                        .as_str()?
                        .strip_suffix("/.codex")
                })
        })
        .unwrap_or(directory)
        .to_string()
}

fn validate_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).context("Invalid Codex public URL")?;
    if url.scheme() != "wss"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Codex's public address must be a WSS origin without embedded credentials");
    }
    Ok(url)
}

async fn verify_connection(connection: &Connection) -> Result<()> {
    let mut endpoint = validate_url(&connection.url)?;
    endpoint
        .set_scheme("https")
        .expect("WSS origin can use HTTPS");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(response) = client.get(endpoint.join("readyz")?).send().await {
            if response.status().is_redirection() {
                bail!("Codex's public endpoint redirected unexpectedly");
            }
            if response.status().is_success() {
                break;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("Codex started, but its public readiness check failed. Rerun connect to retry.");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    verify_websocket(&client, endpoint.as_str(), &connection.token).await
}

async fn verify_websocket(client: &reqwest::Client, endpoint: &str, token: &str) -> Result<()> {
    let unauthenticated = client.get(endpoint).upgrade().send().await?;
    if unauthenticated.status() != reqwest::StatusCode::UNAUTHORIZED {
        bail!("Codex's public endpoint did not reject an unauthenticated WebSocket connection");
    }
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .upgrade()
        .send()
        .await?;
    response.error_for_status_ref()?;
    let mut socket = response
        .into_websocket()
        .await
        .context("Opening authenticated Codex WebSocket")?;
    socket
        .send(Message::Text(
            json!({
                "id": 1, "method": "initialize", "params": {
                    "clientInfo": {"name": "railway_cli", "version": env!("CARGO_PKG_VERSION")}
                }
            })
            .to_string(),
        ))
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = socket.next().await {
            if let Message::Text(text) = frame? {
                let value: serde_json::Value = serde_json::from_str(&text)?;
                if value["id"] != 1 {
                    continue;
                }
                if value.get("error").is_some() || !value["result"].is_object() {
                    bail!("Codex rejected the App Server initialization handshake");
                }
                socket
                    .send(Message::Text(json!({"method": "initialized"}).to_string()))
                    .await?;
                SinkExt::close(&mut socket).await?;
                return Ok(());
            }
        }
        bail!("Codex closed the connection before initialization")
    })
    .await
    .context("Codex initialization timed out")?
}

pub(crate) fn attach_args(connection: &Connection) -> Vec<String> {
    vec![
        // Railway pins the local client to its server. Codex's global-update
        // prompt can steal startup input and would break that version match.
        "-c".into(),
        "check_for_update_on_startup=false".into(),
        "--remote".into(),
        connection.url.clone(),
        "--remote-auth-token-env".into(),
        TOKEN_ENV.into(),
        "--cd".into(),
        connection.directory.clone(),
        "--ask-for-approval".into(),
        "never".into(),
        "--sandbox".into(),
        "danger-full-access".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_targets_the_disabled_repository_layer_including_quoted_paths() {
        let root = "/app/a \"project\"";
        let config = json!({"layers": [{"name": {"type": "project", "dotCodexFolder": format!("{root}/.codex")},
            "disabledReason": format!("Project config is disabled, add {root} as a trusted project in /home/me/.codex/config.toml")}]});
        assert_eq!(trust_target(&config, "/app/a \"project\"/child"), root);
        let key = format!(
            "projects.{}.trust_level = \"trusted\"",
            serde_json::to_string(root).unwrap()
        );
        let parsed: toml::Value = toml::from_str(&key).unwrap();
        assert_eq!(
            parsed["projects"][root]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(trust_target(&json!({}), "/app"), "/app");
    }

    #[tokio::test]
    #[ignore = "requires a locally installed Codex App Server"]
    async fn real_codex_trust_persists_without_replacing_other_config() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("codex-home");
        let project = root.path().join("a project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        std::fs::write(home.join("config.toml"), "# retained\nmodel = \"gpt-5\"\n").unwrap();
        std::fs::write(
            project.join(".codex/config.toml"),
            "model_reasoning_effort = \"high\"\n",
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut server = tokio::process::Command::new("codex")
            .args(["app-server", "--listen", &format!("ws://{address}")])
            .env("CODEX_HOME", &home)
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut rpc = loop {
            match Rpc::open(&format!("http://{address}"), "").await {
                Ok(rpc) => break rpc,
                Err(error) => {
                    assert!(tokio::time::Instant::now() < deadline, "{error:#}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        let directory = project
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        trust_with(&mut rpc, &directory).await.unwrap();
        trust_with(&mut rpc, &directory).await.unwrap();
        let stored = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(stored.contains("# retained"));
        let parsed: toml::Value = toml::from_str(&stored).unwrap();
        assert_eq!(parsed["model"].as_str(), Some("gpt-5"));
        assert_eq!(
            parsed["projects"][&directory]["trust_level"].as_str(),
            Some("trusted")
        );
        let effective = rpc
            .call("config/read", json!({"cwd":directory,"includeLayers":true}))
            .await
            .unwrap();
        rpc.call("thread/list", json!({"limit":100,"sortKey":"updated_at","archived":false,"sourceKinds":["cli","vscode","appServer","exec"]})).await.unwrap();
        assert!(
            effective["layers"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|l| l["name"]["type"] == "project")
                .all(|l| l["disabledReason"].is_null())
        );
        server.kill().await.unwrap();
    }

    #[test]
    fn public_endpoint_requires_tls_and_separate_auth() {
        for invalid in [
            "ws://host",
            "https://host",
            "wss://user:secret@host",
            "wss://host/path",
            "wss://host?token=secret",
            "wss://host#fragment",
        ] {
            assert!(validate_url(invalid).is_err(), "{invalid}");
        }
        assert!(validate_url("wss://agent.example.com").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn bootstrap_preserves_server_identity_and_authentication() {
        let output = std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/codex_server.py"
            ))
            .output()
            .expect("python3 is required for Codex bootstrap tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
