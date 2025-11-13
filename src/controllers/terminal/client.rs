use anyhow::{Result, bail};
use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::Message;
use futures_util::stream::StreamExt;
use indicatif::ProgressBar;
use std::io::Write;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval, timeout};

use crate::commands::ssh::{AuthKind, SSH_MAX_EMPTY_MESSAGES, SSH_MESSAGE_TIMEOUT_SECS};

use super::SSH_PING_INTERVAL_SECS;
use super::connection::{SSHConnectParams, establish_connection};
use super::messages::{ClientMessage, ClientPayload, DataPayload, ServerMessage};

pub struct TerminalClient {
    ws_stream: WebSocketStream<async_tungstenite::tokio::ConnectStream>,
    initialized: bool,
    ready: bool,
    in_command_progress: bool,
    ready_tx: Option<mpsc::Sender<bool>>,
}

impl TerminalClient {
    pub async fn new(
        url: &str,
        token: AuthKind,
        params: &SSHConnectParams,
        spinner: &mut ProgressBar,
        max_attempts: Option<u32>,
    ) -> Result<Self> {
        // Use the correct establish_connection function that handles authentication
        let ws_stream = establish_connection(url, token, params, spinner, max_attempts).await?;

        let mut client = Self {
            ws_stream,
            initialized: false,
            ready: false,
            in_command_progress: false,
            ready_tx: None,
        };

        // Wait for the initial welcome message from the server
        if let Some(msg_result) = client.ws_stream.next().await {
            let msg = msg_result.map_err(|e| anyhow::anyhow!("WebSocket error: {}", e))?;

            if let Message::Text(text) = msg {
                let server_msg: ServerMessage = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("Failed to parse server message: {}", e))?;

                if server_msg.r#type != "welcome" {
                    bail!("Expected welcome message, received: {:?}", server_msg);
                }

                Ok(client)
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

        // Do not send data when in command progress (stand_by) mode
        if self.in_command_progress {
            return Ok(());
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
        // Allow window resize before initialization (needed for initial setup)
        // But block it when initialized but not yet ready
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
    pub async fn handle_server_messages_with_writer<W: Write>(
        &mut self,
        writer: &mut W,
        fail_on_exit_code: bool,
    ) -> Result<i32> {
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
                                                if !text.trim().is_empty() {
                                                    writer.write_all(text.as_bytes())?;
                                                    writer.flush()?;
                                                } else {
                                                    consecutive_empty_messages = 0;
                                                }
                                            }
                                            DataPayload::Buffer { data } => {
                                                if !data.is_empty() {
                                                    writer.write_all(&data)?;
                                                    writer.flush()?;
                                                } else {
                                                    consecutive_empty_messages = 0;
                                                }
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
                                            self.in_command_progress = false;

                                            // Notify waiting functions that the shell is ready
                                            if let Some(tx) = &self.ready_tx {
                                                let _ = tx.send(true).await;
                                            }
                                        },
                                        "stand_by" => {
                                            // This indicates command is in progress
                                            self.ready = true;
                                            self.in_command_progress = true;

                                            // Notify waiting functions that the shell is ready
                                            if let Some(tx) = &self.ready_tx {
                                                let _ = tx.send(true).await;
                                            }
                                        },
                                        "command_exit" => {
                                            if let Some(code) = server_msg.payload.code {
                                                writer.flush()?;
                                                // If exit code is non-zero, exit with that code
                                                if code != 0 {
                                                    if fail_on_exit_code {
                                                        std::process::exit(code);
                                                    } else {
                                                        bail!("Command exited with code: {}", code);
                                                    }
                                                }

                                                return Ok(code);
                                            }
                                        },
                                        "error" => {
                                            // Notify waiting functions that the shell initialization failed
                                            if let Some(tx) = &self.ready_tx {
                                                let _ = tx.send(false).await;
                                            }
                                            bail!(server_msg.payload.message);
                                        }
                                        "welcome" => {
                                            // Ignore welcome messages after initialization
                                        }
                                        "pty_closed" => {
                                            return Ok(0);
                                        }
                                        unknown_type => {
                                            writeln!(writer, "Warning: Received unknown message type: {unknown_type}")?;
                                        }
                                    }
                                }
                                Message::Close(frame) => {
                                    if let Some(tx) = &self.ready_tx {
                                        let _ = tx.send(false).await;
                                    }
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
                                    writeln!(writer, "Warning: Unexpected binary message received")?;
                                }
                                Message::Frame(_) => {
                                    writeln!(writer, "Warning: Unexpected raw frame received")?;
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

    /// Process incoming messages from the server and write to stdout
    pub async fn handle_server_messages(&mut self) -> Result<()> {
        self.handle_server_messages_with_writer(&mut std::io::stdout(), true)
            .await?;

        Ok(())
    }

    /// Directly waits for a ready or stand_by message from the server
    pub async fn wait_for_shell_ready(&mut self, timeout_secs: u64) -> Result<()> {
        let timeout_future = tokio::time::sleep(Duration::from_secs(timeout_secs));
        tokio::pin!(timeout_future);

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

                                    if server_msg.r#type == "ready" || server_msg.r#type == "stand_by" {
                                        self.ready = true;
                                        self.in_command_progress = server_msg.r#type == "stand_by";
                                        return Ok(());
                                    }

                                    // Handle specially recognized messages
                                    match server_msg.r#type.as_str() {
                                        "session_data" => match server_msg.payload.data {
                                            DataPayload::String(text) => {
                                                if !text.trim().is_empty() {
                                                    print!("{text}");
                                                    std::io::stdout().flush()?;
                                                }
                                            }
                                            DataPayload::Buffer { data } => {
                                                if !data.is_empty() {
                                                    std::io::stdout().write_all(&data)?;
                                                    std::io::stdout().flush()?;
                                                }
                                            }
                                            DataPayload::Empty {} => {}
                                        },
                                        "error" => {
                                            bail!("Error from server: {}", server_msg.payload.message);
                                        }
                                        _ => {}
                                    }
                                },
                                // Handle other message types
                                Message::Ping(data) => {
                                    self.send_message(Message::Pong(data)).await?;
                                }
                                Message::Close(frame) => {
                                    if let Some(frame) = frame {
                                        bail!("WebSocket closed with code {}: {}", frame.code, frame.reason);
                                    } else {
                                        bail!("WebSocket closed unexpectedly");
                                    }
                                }
                                // Ignore other message types
                                _ => {}
                            }
                        },
                        None => {
                            bail!("WebSocket connection closed unexpectedly");
                        }
                    }
                },
                _ = &mut timeout_future => {
                    bail!("Timed out waiting for shell to be ready");
                }
            }
        }
    }

    /// Check if the shell is ready for input
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Check if the shell is ready and not currently processing a command
    pub fn is_ready_for_input(&self) -> bool {
        self.ready && !self.in_command_progress
    }
}
