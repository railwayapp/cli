use anyhow::{bail, Result};
use async_tungstenite::tungstenite::handshake::client::generate_key;
use async_tungstenite::tungstenite::http::Request;
use async_tungstenite::{tungstenite::Message, WebSocketStream};
use futures_util::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::io::Write;
use tokio::time::{interval, sleep, timeout, Duration};
use url::Url;

use crate::commands::{
    SSH_CONNECTION_TIMEOUT_SECS, SSH_MAX_EMPTY_MESSAGES, SSH_MAX_RECONNECT_ATTEMPTS,
    SSH_MESSAGE_TIMEOUT_SECS, SSH_RECONNECT_DELAY_SECS,
};

const SSH_PING_INTERVAL_SECS: u64 = 10;

#[derive(Clone, Debug)]
pub struct SSHConnectParams {
    pub project_id: String,
    pub environment_id: String,
    pub service_id: String,
    pub deployment_instance_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ClientMessage {
    pub r#type: String,
    pub payload: ClientPayload,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ClientPayload {
    Data { data: String },
    WindowSize { cols: u16, rows: u16 },
    Signal { signal: u8 },
}

#[derive(Debug, Deserialize)]
struct ServerMessage {
    r#type: String,
    payload: ServerPayload,
}

#[derive(Debug, Deserialize)]
struct ServerPayload {
    #[serde(default)]
    data: DataPayload,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DataPayload {
    String(String),
    Buffer { data: Vec<u8> },
    Empty {},
}

impl Default for DataPayload {
    fn default() -> Self {
        DataPayload::Empty {}
    }
}

pub struct TerminalClient {
    ws_stream: WebSocketStream<async_tungstenite::tokio::ConnectStream>,
}

impl TerminalClient {
    pub async fn new(url: &str, token: &str, params: &SSHConnectParams) -> Result<Self> {
        let url = Url::parse(url)?;

        for attempt in 1..=SSH_MAX_RECONNECT_ATTEMPTS {
            match Self::attempt_connection(&url, token, params).await {
                Ok(ws_stream) => {
                    return Ok(Self { ws_stream });
                }
                Err(e) => {
                    if attempt == SSH_MAX_RECONNECT_ATTEMPTS {
                        bail!(
                            "Failed to establish connection after {} attempts: {}",
                            SSH_MAX_RECONNECT_ATTEMPTS,
                            e
                        );
                    }
                    eprintln!(
                        "Connection attempt {} failed: {}. Retrying in {} seconds...",
                        attempt, e, SSH_RECONNECT_DELAY_SECS
                    );
                    sleep(Duration::from_secs(SSH_RECONNECT_DELAY_SECS)).await;
                }
            }
        }

        bail!("Failed to establish connection after all attempts");
    }
    async fn attempt_connection(
        url: &Url,
        token: &str,
        params: &SSHConnectParams,
    ) -> Result<WebSocketStream<async_tungstenite::tokio::ConnectStream>> {
        let key = generate_key();

        let mut request = Request::builder()
            .uri(url.as_str())
            .header("Authorization", format!("Bearer {}", token))
            .header("Sec-WebSocket-Key", key)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header("Host", url.host_str().unwrap_or(""))
            .header("X-Railway-Project-Id", params.project_id.clone())
            .header("X-Railway-Service-Id", params.service_id.clone())
            .header("X-Railway-Environment-Id", params.environment_id.clone());

        if let Some(instance_id) = params.deployment_instance_id.as_ref() {
            request = request.header("X-Railway-Deployment-Instance-Id", instance_id);
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
    async fn send_message(&mut self, msg: Message) -> Result<()> {
        timeout(
            Duration::from_secs(SSH_MESSAGE_TIMEOUT_SECS),
            self.ws_stream.send(msg),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Message send timed out after {} seconds",
                SSH_MESSAGE_TIMEOUT_SECS
            )
        })??;
        Ok(())
    }

    pub async fn send_data(&mut self, data: &str) -> Result<()> {
        let message = ClientMessage {
            r#type: "session_data".to_string(),
            payload: ClientPayload::Data {
                data: data.to_string(),
            },
        };

        let msg = serde_json::to_string(&message)?;
        self.send_message(Message::Text(msg))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send data: {}", e))?;
        Ok(())
    }

    pub async fn send_window_size(&mut self, cols: u16, rows: u16) -> Result<()> {
        let message = ClientMessage {
            r#type: "window_resize".to_string(),
            payload: ClientPayload::WindowSize { cols, rows },
        };

        let msg = serde_json::to_string(&message)?;
        self.send_message(Message::Text(msg))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send window size: {}", e))?;
        Ok(())
    }

    pub async fn send_signal(&mut self, signal: u8) -> Result<()> {
        let message = ClientMessage {
            r#type: "signal".to_string(),
            payload: ClientPayload::Signal { signal },
        };

        let msg = serde_json::to_string(&message)?;
        self.send_message(Message::Text(msg))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send signal: {}", e))?;
        Ok(())
    }

    async fn send_ping(&mut self) -> Result<()> {
        self.send_message(Message::Ping(vec![]))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send ping: {}", e))?;
        Ok(())
    }

    pub async fn handle_server_messages(&mut self) -> Result<()> {
        let mut consecutive_empty_messages = 0;

        let mut ping_interval = interval(Duration::from_secs(SSH_PING_INTERVAL_SECS));

        loop {
            tokio::select! {
                msg_option = self.ws_stream.next() => {
                    match msg_option {
                        Some(msg_result) => {
                            let msg = msg_result.map_err(|e| anyhow::anyhow!("WebSocket error: {}", e))?;

                            match msg {
                                Message::Text(text) => {
                                    let server_msg: ServerMessage = serde_json::from_str(&text)
                                        .map_err(|e| anyhow::anyhow!("Failed to parse server message: {}", e))?;

                                    match server_msg.r#type.as_str() {
                                        "session_data" => match server_msg.payload.data {
                                            DataPayload::String(text) => {
                                                consecutive_empty_messages = 0;
                                                print!("{}", text);
                                                std::io::stdout().flush()?;
                                            }
                                            DataPayload::Buffer { data } => {
                                                consecutive_empty_messages = 0;
                                                std::io::stdout().write_all(&data)?;
                                                std::io::stdout().flush()?;
                                            }
                                            DataPayload::Empty {} => {
                                                consecutive_empty_messages += 1;
                                                if consecutive_empty_messages >= SSH_MAX_EMPTY_MESSAGES {
                                                    bail!("Received too many empty messages in a row, connection may be stale");
                                                }
                                            }
                                        },
                                        "error" => {
                                            bail!(server_msg.payload.message);
                                        }
                                        "pty_closed" => {
                                            return Ok(());
                                        }
                                        unknown_type => {
                                            eprintln!("Warning: Received unknown message type: {}", unknown_type);
                                        }
                                    }
                                }
                                Message::Close(frame) => {
                                    if let Some(frame) = frame {
                                        bail!(
                                            "WebSocket closed with code {}: {}",
                                            frame.code,
                                            frame.reason
                                        );
                                    } else {
                                        bail!("WebSocket closed unexpectedly");
                                    }
                                }
                                Message::Ping(data) => {
                                    self.send_message(Message::Pong(data)).await?;
                                }
                                Message::Pong(data) => {
                                    // Pong recevied
                                }
                                Message::Binary(_) => {
                                    eprintln!("Warning: Unexpected binary message received");
                                }
                                Message::Frame(_) => {
                                    eprintln!("Warning: Unexpected raw frame received");
                                }
                            }
                        },
                        None => {
                            bail!("WebSocket connection closed unexpectedly");
                        }
                    }
                },

                _ = ping_interval.tick() => {
                    self.send_ping().await?;
                }
            }
        }
    }
}
