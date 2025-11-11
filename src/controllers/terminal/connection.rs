use anyhow::{Result, bail};
use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::handshake::client::generate_key;
use async_tungstenite::tungstenite::http::Request;
use indicatif::ProgressBar;
use tokio::time::{Duration, sleep, timeout};
use url::Url;

use crate::consts::get_user_agent;
use crate::{
    commands::ssh::{
        AuthKind, SSH_CONNECT_DELAY_SECS, SSH_CONNECTION_TIMEOUT_SECS, SSH_MAX_CONNECT_ATTEMPTS,
    },
    errors::RailwayError,
};

#[derive(Clone, Debug)]
pub struct SSHConnectParams {
    pub project_id: String,
    pub environment_id: String,
    pub service_id: String,
    pub deployment_instance_id: Option<String>,
}

/// Establishes a WebSocket connection
pub async fn establish_connection(
    url: &str,
    token: AuthKind,
    params: &SSHConnectParams,
    spinner: &mut ProgressBar,
    max_attempts: Option<u32>,
) -> Result<WebSocketStream<async_tungstenite::tokio::ConnectStream>> {
    let url = Url::parse(url)?;

    let max_attempts = max_attempts.unwrap_or(SSH_MAX_CONNECT_ATTEMPTS);

    for attempt in 1..=max_attempts {
        match attempt_connection(&url, token.clone(), params).await {
            Ok(ws_stream) => {
                return Ok(ws_stream);
            }
            Err(e) => {
                if attempt == max_attempts {
                    bail!(
                        "Failed to establish connection after {} attempts: {}",
                        max_attempts,
                        e
                    );
                }

                spinner.set_message(format!(
                    "Connection attempt {attempt} failed: {e}. Retrying in {SSH_CONNECT_DELAY_SECS} seconds..."
                ));

                sleep(Duration::from_secs(SSH_CONNECT_DELAY_SECS)).await;
            }
        }
    }

    bail!("Failed to establish connection after all attempts");
}

/// Attempts to establish a single WebSocket connection
pub async fn attempt_connection(
    url: &Url,
    token: AuthKind,
    params: &SSHConnectParams,
) -> Result<WebSocketStream<async_tungstenite::tokio::ConnectStream>> {
    let key = generate_key();

    let mut request = Request::builder()
        .uri(url.as_str())
        .header("Sec-WebSocket-Key", key)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Version", "13")
        .header("Host", url.host_str().unwrap_or(""))
        .header("X-Source", get_user_agent())
        .header("X-Railway-Project-Id", params.project_id.clone())
        .header("X-Railway-Service-Id", params.service_id.clone())
        .header("X-Railway-Environment-Id", params.environment_id.clone());

    if let Some(instance_id) = params.deployment_instance_id.as_ref() {
        request = request.header("X-Railway-Deployment-Instance-Id", instance_id);
    }
    match token {
        AuthKind::Bearer(token) => {
            request = request.header("Authorization", format!("Bearer {token}"))
        }
        AuthKind::ProjectAccessToken(token) => {
            request = request.header("project-access-token", token)
        }
    }

    let request = request.body(())?;

    let (ws_stream, response) = timeout(
        Duration::from_secs(SSH_CONNECTION_TIMEOUT_SECS),
        async_tungstenite::tokio::connect_async_with_config(request, None),
    )
    .await??;

    if response.status().as_u16() == 101 {
        Ok(ws_stream)
    } else {
        bail!(
            "Server did not upgrade to WebSocket. Status: {}",
            response.status()
        );
    }
}
