use anyhow::{bail, Result};
use async_tungstenite::tungstenite::Message;
use async_tungstenite::WebSocketStream;
use futures_util::stream::StreamExt;
use std::io::Write;
use tokio::time::{interval, timeout, Duration};

use crate::commands::ssh::{SSH_MAX_EMPTY_MESSAGES, SSH_MESSAGE_TIMEOUT_SECS};

use super::connection::{establish_connection, SSHConnectParams};
use super::messages::{ClientMessage, ClientPayload, DataPayload, ServerMessage};
use super::SSH_PING_INTERVAL_SECS;

pub struct TerminalClient {
    ws_stream: WebSocketStream<async_tungstenite::tokio::ConnectStream>,
    initialized: bool,
    ready: bool,
}

impl TerminalClient {
    pub async fn new(url: &str, token: &str, params: &SSHConnectParams) -> Result<Self> {
        let ws_stream = establish_connection(url, token, params).await?;

        let mut client = Self {
            ws_stream,
            initialized: false,
            ready: false,
        };

        // Wait for the initial welcome message from the server
        if let Some(msg_result) = client.ws_stream.next().await {
            let msg = msg_result.map_err(|e| anyhow::anyhow!("WebSocket error: {}", e))?;

            if let Message::Text(text) = msg {
                let server_msg: ServerMessage = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("Failed to parse server message: {}", e))?;

                if server_msg.r#type != "welcome" {
                    bail!("Expected welcome message, received: {}", server_msg.r#type);
                }

                return Ok(client);
            } else {
                bail!("Expected text message for welcome, received different message type");
            }
        } else {
            bail!("Connection closed before receiving welcome message");
        }
    }

    /// Sends a WebSocket message
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

    /// Initializes an interactive shell session
    pub async fn init_shell(&mut self, shell: Option<String>) -> Result<()> {
        if self.initialized {
            bail!("Session already initialized");
        }

        let message = ClientMessage {
            r#type: "init_shell".to_string(),
            payload: ClientPayload::InitShell { shell },
        };

        let msg = serde_json::to_string(&message)?;
        self.send_message(Message::Text(msg))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to initialize shell: {}", e))?;

        self.initialized = true;
        self.ready = false;

        // Wait for the ready response
        let timeout_duration = Duration::from_secs(10); // 10 seconds timeout
        let mut wait_time = Duration::from_secs(0);
        let tick_duration = Duration::from_millis(100);

        while !self.ready {
            if wait_time >= timeout_duration {
                bail!("Timed out waiting for ready response from server");
            }

            if let Some(msg_result) = timeout(tick_duration, self.ws_stream.next()).await? {
                let msg = msg_result.map_err(|e| anyhow::anyhow!("WebSocket error: {}", e))?;

                if let Message::Text(text) = msg {
                    let server_msg: ServerMessage = serde_json::from_str(&text)
                        .map_err(|e| anyhow::anyhow!("Failed to parse server message: {}", e))?;

                    match server_msg.r#type.as_str() {
                        "ready" => {
                            self.ready = true;
                            break;
                        }
                        "session_data" => {
                            // Echo any data received while waiting for ready
                            match server_msg.payload.data {
                                DataPayload::String(text) => {
                                    print!("{}", text);
                                    std::io::stdout().flush()?;
                                }
                                DataPayload::Buffer { data } => {
                                    std::io::stdout().write_all(&data)?;
                                    std::io::stdout().flush()?;
                                }
                                DataPayload::Empty {} => {}
                            }
                        }
                        "error" => {
                            bail!("Error initializing shell: {}", server_msg.payload.message);
                        }
                        _ => {
                            // Ignore other message types while waiting for ready
                        }
                    }
                }
            } else {
                bail!("Connection closed while waiting for ready response");
            }

            wait_time += tick_duration;
        }

        Ok(())
    }

    /// Executes a single command
    pub async fn send_command(&mut self, command: &str, args: Vec<String>) -> Result<()> {
        if self.initialized {
            bail!("Session already initialized");
        }

        let message = ClientMessage {
            r#type: "exec_command".to_string(),
            payload: ClientPayload::Command {
                command: command.to_string(),
                args,
                env: std::collections::HashMap::new(),
            },
        };

        let msg = serde_json::to_string(&message)?;
        self.send_message(Message::Text(msg))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))?;

        self.initialized = true;
        self.ready = true; // Exec commands are immediately ready

        Ok(())
    }

    /// Sends data to the terminal session
    pub async fn send_data(&mut self, data: &str) -> Result<()> {
        if !self.initialized {
            bail!("Session not initialized");
        }

        if !self.ready {
            bail!("Shell not ready yet");
        }

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

    /// Updates the terminal window size
    pub async fn send_window_size(&mut self, cols: u16, rows: u16) -> Result<()> {
        if self.initialized && !self.ready {
            bail!("Shell not ready yet");
        }

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

    /// Sends a signal to the terminal session
    pub async fn send_signal(&mut self, signal: u8) -> Result<()> {
        if !self.initialized {
            bail!("Session not initialized");
        }

        if !self.ready {
            bail!("Shell not ready yet");
        }

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

    /// Sends a ping message to keep the connection alive
    async fn send_ping(&mut self) -> Result<()> {
        self.send_message(Message::Ping(vec![]))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send ping: {}", e))?;
        Ok(())
    }

    /// Process incoming messages from the server
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
                                        "ready" => {
                                            // Client can start sending data/events
                                            self.ready = true;
                                        },
                                        "stand_by" => {
                                            // This indicates command is in progress
                                            self.ready = true;
                                        },
                                        "command_exit" => {
                                            if let Some(code) = server_msg.payload.code {
                                                std::io::stdout().flush()?;
                                                // If exit code is non-zero, exit with that code
                                                if code != 0 {
                                                    std::process::exit(code);
                                                }
                                                return Ok(());
                                            }
                                        },
                                        "error" => {
                                            bail!(server_msg.payload.message);
                                        }
                                        "welcome" => {
                                            // Ignore welcome messages after initialization
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
                                Message::Pong(_) => {
                                    // Pong received, connection is still alive
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

    /// Check if the shell is ready for input
    pub fn is_ready(&self) -> bool {
        self.ready
    }
}

