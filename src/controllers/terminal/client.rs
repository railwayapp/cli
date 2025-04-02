use anyhow::{bail, Result};
use async_tungstenite::tungstenite::{Error as WsError, Message};
use async_tungstenite::WebSocketStream;
use futures_util::stream::StreamExt;
use std::io::Write;
use std::time::{Duration as StdDuration, Instant};
use tokio::time::{interval, sleep, timeout, Duration};

use crate::commands::ssh::{
    SSH_IDLE_THRESHOLD_SECS, SSH_MAX_EMPTY_MESSAGES, SSH_MAX_RECONNECT_ATTEMPTS,
    SSH_MESSAGE_TIMEOUT_SECS, SSH_RECONNECT_DELAY_MS,
};

use super::connection::{establish_connection, SSHConnectParams};
use super::messages::{ClientMessage, ClientPayload, DataPayload, ServerMessage};
use super::SSH_PING_INTERVAL_SECS;

struct ReconnectGuard<'a>(&'a std::sync::atomic::AtomicBool);

impl<'a> Drop for ReconnectGuard<'a> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub struct TerminalClient {
    ws_stream: WebSocketStream<async_tungstenite::tokio::ConnectStream>,
    initialized: bool,
    url: String,
    token: String,
    params: SSHConnectParams,
    last_activity: Instant,
    reconnecting: std::sync::atomic::AtomicBool,
    reconnect_attempts: u8,
    last_reconnect_time: Option<Instant>,
    ready: bool,
    exec_command_mode: bool,
}

impl TerminalClient {
    pub async fn new(url: &str, token: &str, params: &SSHConnectParams) -> Result<Self> {
        let ws_stream = establish_connection(url, token, params).await?;

        let mut client = Self {
            ws_stream,
            initialized: false,
            url: url.to_string(),
            token: token.to_string(),
            params: params.clone(),
            last_activity: Instant::now(),
            reconnecting: std::sync::atomic::AtomicBool::new(false),
            reconnect_attempts: 0,
            last_reconnect_time: None,
            ready: false,
            exec_command_mode: false,
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

    /// Check if the connection has been idle for the threshold period
    fn is_idle(&self) -> bool {
        Instant::now().duration_since(self.last_activity)
            >= StdDuration::from_secs(SSH_IDLE_THRESHOLD_SECS)
    }

    /// Check if we've recently attempted a reconnect
    /// This helps us handle quick failures after reconnecting
    fn is_recent_reconnect(&self) -> bool {
        if let Some(last_time) = self.last_reconnect_time {
            Instant::now().duration_since(last_time) < StdDuration::from_secs(5)
        } else {
            false
        }
    }

    /// Check if reconnection is allowed in the current mode
    fn can_reconnect(&self) -> bool {
        // Only allow reconnection in interactive shell mode, not in exec_command mode
        !self.exec_command_mode
    }

    /// Update the last activity timestamp
    fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Attempts to reconnect to the server
    async fn reconnect(&mut self) -> Result<()> {
        // Skip reconnection if we're in exec_command mode
        if !self.can_reconnect() {
            bail!("Reconnection not allowed in exec_command mode");
        }

        // Don't allow concurrent reconnections
        if self
            .reconnecting
            .swap(true, std::sync::atomic::Ordering::Acquire)
        {
            return Err(anyhow::anyhow!("Reconnection already in progress"));
        }

        // Create a guard that will reset the flag when this function scope ends
        let _guard = ReconnectGuard(&self.reconnecting);

        // Only try to reconnect if idle OR if we recently reconnected (handles quick failures)
        if !self.is_idle() && !self.is_recent_reconnect() {
            bail!("Connection issue, but not idle enough for reconnection");
        }

        // Check if we've exceeded the maximum reconnection attempts
        if self.reconnect_attempts >= SSH_MAX_RECONNECT_ATTEMPTS {
            bail!(
                "Maximum reconnection attempts ({}) reached",
                SSH_MAX_RECONNECT_ATTEMPTS
            );
        }

        // Increment the reconnection attempt counter
        self.reconnect_attempts += 1;

        // Update the last reconnect time
        self.last_reconnect_time = Some(Instant::now());

        // Add a small delay between reconnection attempts
        if self.reconnect_attempts > 1 {
            sleep(Duration::from_millis(SSH_RECONNECT_DELAY_MS)).await;
        }

        match establish_connection(&self.url, &self.token, &self.params).await {
            Ok(new_ws_stream) => {
                self.ws_stream = new_ws_stream;
                self.ready = false;

                // Wait for welcome message
                if let Some(msg_result) = self.ws_stream.next().await {
                    if let Ok(Message::Text(text)) = msg_result {
                        if let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) {
                            if server_msg.r#type == "welcome" {
                                // Reset initialized state
                                self.initialized = false;

                                // Clear the current line completely and move to beginning of line
                                print!("\r\x1B[K");
                                std::io::stdout().flush().ok();

                                // Ensure cursor is visible
                                print!("\x1b[?25h"); // Show cursor escape sequence
                                std::io::stdout().flush().ok();

                                // Signal that we need re-initialization
                                bail!("reconnected but needs re-initialization");
                            }
                        }
                    }
                }
                bail!("Didn't receive proper welcome message after reconnection");
            }
            Err(e) => {
                bail!(
                    "Failed to reconnect (attempt {}/{}): {}",
                    self.reconnect_attempts,
                    SSH_MAX_RECONNECT_ATTEMPTS,
                    e
                );
            }
        }
    }

    /// Sends a WebSocket message
    async fn send_message(&mut self, msg: Message) -> Result<()> {
        // Check if the message is a ping for special handling afterwards
        let is_ping = matches!(msg, Message::Ping(_));

        if !self.initialized && !is_ping && !matches!(msg, Message::Pong(_)) {
            if let Message::Text(text) = &msg {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
                    if let Some(msg_type) = value.get("type").and_then(|v| v.as_str()) {
                        if msg_type != "window_resize"
                            && msg_type != "init_shell"
                            && msg_type != "exec_command"
                        {
                            bail!("Session not initialized");
                        }
                    } else {
                        bail!("Session not initialized");
                    }
                } else {
                    bail!("Session not initialized");
                }
            } else {
                bail!("Session not initialized");
            }
        }

        // Don't update activity time for pings to avoid interfering with idle detection
        if !is_ping {
            self.update_activity();
        }

        match timeout(
            Duration::from_secs(SSH_MESSAGE_TIMEOUT_SECS),
            self.ws_stream.send(msg),
        )
        .await
        {
            Ok(Ok(_)) => {
                // On successful message, reset reconnect attempts if it's not a ping
                // We want to keep the counter for recent quick failures
                if !is_ping && !self.is_recent_reconnect() {
                    self.reconnect_attempts = 0;
                }
                Ok(())
            }
            Ok(Err(e)) => {
                // If connection error, try to reconnect
                match &e {
                    WsError::ConnectionClosed
                    | WsError::AlreadyClosed
                    | WsError::Protocol(_)
                    | WsError::Io(_) => {
                        // Only try to reconnect if we're in interactive mode
                        if self.can_reconnect() {
                            self.reconnect().await
                        } else {
                            Err(anyhow::anyhow!(
                                "Connection error in exec_command mode: {}",
                                e
                            ))
                        }
                    }
                    _ => Err(anyhow::anyhow!("Failed to send message: {}", e)),
                }
            }
            Err(_) => Err(anyhow::anyhow!(
                "Message send timed out after {} seconds",
                SSH_MESSAGE_TIMEOUT_SECS
            )),
        }
    }

    /// Initializes an interactive shell session and waits for the ready response
    pub async fn init_shell(&mut self, shell: Option<String>) -> Result<()> {
        // Set to interactive shell mode
        self.exec_command_mode = false;

        // Allow re-initialization
        if self.initialized && shell.is_none() {
            return Ok(());
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
        let start_time = Instant::now();

        while !self.ready {
            if start_time.elapsed() > StdDuration::from_secs(10) {
                bail!("Timed out waiting for ready response from server");
            }

            if let Some(msg_result) = timeout(timeout_duration, self.ws_stream.next()).await? {
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
        }

        Ok(())
    }

    /// Executes a single command
    pub async fn send_command(&mut self, command: &str, args: Vec<String>) -> Result<()> {
        self.exec_command_mode = true;

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
        self.ready = true;

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
                        Some(Ok(msg)) => {
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
                                                // Reset reconnect attempts on successful data
                                                // only if not a recent reconnect
                                                if !self.is_recent_reconnect() {
                                                    self.reconnect_attempts = 0;
                                                }
                                                self.update_activity();
                                            }
                                            DataPayload::Buffer { data } => {
                                                consecutive_empty_messages = 0;
                                                std::io::stdout().write_all(&data)?;
                                                std::io::stdout().flush()?;
                                                // Update activity when receiving data
                                                self.update_activity();
                                                // Reset reconnect attempts on successful data
                                                // only if not a recent reconnect
                                                if !self.is_recent_reconnect() {
                                                    self.reconnect_attempts = 0;
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
                                            self.update_activity();
                                        },
                                        "stand_by" => {
                                            // This indicates command is in progress
                                            self.ready = true;
                                            self.update_activity();
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
                                            // Check if this is the specific connection closed error
                                            if server_msg.payload.message.contains("Connection to application unexpectedly closed") {
                                                // Only try to reconnect if we're in interactive mode and either idle or recently reconnected
                                                if self.can_reconnect() && (self.is_idle() || self.is_recent_reconnect()) {
                                                    // Try to reconnect
                                                    match self.reconnect().await {
                                                        Ok(()) => {}, // Successfully reconnected
                                                        Err(reconnect_err) => {
                                                            if reconnect_err.to_string().contains("reconnected but needs re-initialization") {
                                                                bail!("{}", reconnect_err);
                                                            }
                                                            if reconnect_err.to_string().contains("Maximum reconnection attempts") {
                                                                bail!("Connection to application closed. (Max reconnects reached)");
                                                            }
                                                            bail!("Connection to application closed. (Reconnect failed: {})", reconnect_err);
                                                        }
                                                    }
                                                } else {
                                                    // Not in interactive mode or not idle, so just report the error
                                                    bail!(server_msg.payload.message);
                                                }
                                            } else {
                                                // This is some other error, bail directly
                                                bail!(server_msg.payload.message);
                                            }
                                        },
                                        "welcome" => {
                                            // If we get a welcome message and we're already initialized,
                                            // it could mean the server restarted our session.
                                            if self.initialized {
                                                self.initialized = false;
                                                self.ready = false;
                                                bail!("reconnected but needs re-initialization");
                                            }
                                            // Reset reconnect attempts on welcome message
                                            // only if it's been some time since the last reconnect
                                            if !self.is_recent_reconnect() {
                                                self.reconnect_attempts = 0;
                                            }
                                            self.update_activity();
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
                                    // Only try to reconnect if we're in interactive mode
                                    if self.can_reconnect() {
                                        // Try to reconnect on close
                                        match self.reconnect().await {
                                            Ok(()) => {},
                                            Err(e) => {
                                                if e.to_string().contains("reconnected but needs re-initialization") {
                                                    bail!("{}", e);
                                                }
                                                if e.to_string().contains("Maximum reconnection attempts") {
                                                    if let Some(frame) = frame {
                                                        bail!("WebSocket closed with code {}: {} (Max reconnects reached)",
                                                            frame.code, frame.reason);
                                                    } else {
                                                        bail!("WebSocket closed unexpectedly (Max reconnects reached)");
                                                    }
                                                }
                                                if let Some(frame) = frame {
                                                    bail!("WebSocket closed with code {}: {} (Reconnect failed: {})",
                                                        frame.code, frame.reason, e);
                                                } else {
                                                    bail!("WebSocket closed unexpectedly and reconnect failed: {}", e);
                                                }
                                            }
                                        }
                                    } else {
                                        // In exec_command mode, just report the close
                                        if let Some(frame) = frame {
                                            bail!("WebSocket closed with code {}: {} (exec_command mode)",
                                                frame.code, frame.reason);
                                        } else {
                                            bail!("WebSocket closed unexpectedly (exec_command mode)");
                                        }
                                    }
                                }
                                Message::Ping(data) => {
                                    self.update_activity();
                                    if let Err(e) = self.send_message(Message::Pong(data)).await {
                                        if e.to_string().contains("reconnected but needs re-initialization") {
                                            bail!("{}", e);
                                        }
                                        if e.to_string().contains("Maximum reconnection attempts") {
                                            bail!("Max reconnects reached: {}", e);
                                        }
                                        return Err(e);
                                    }
                                }
                                Message::Pong(_) => {
                                    // Pong received, connection is still alive
                                    // Don't update activity time for pongs
                                }
                                _ => {}
                            }
                        },
                        Some(Err(e)) => {
                            // Only try to reconnect if we're in interactive mode
                            if self.can_reconnect() {
                                // Try to reconnect on error
                                match self.reconnect().await {
                                    Ok(()) => {},
                                    Err(reconnect_err) => {
                                        if reconnect_err.to_string().contains("reconnected but needs re-initialization") {
                                            bail!("{}", reconnect_err);
                                        }
                                        if reconnect_err.to_string().contains("Maximum reconnection attempts") {
                                            bail!("WebSocket error: {} (Max reconnects reached)", e);
                                        }
                                        bail!("WebSocket error: {} (Reconnect failed: {})", e, reconnect_err);
                                    }
                                }
                            } else {
                                // In exec_command mode, just report the error
                                bail!("WebSocket error in exec_command mode: {}", e);
                            }
                        },
                        None => {
                            // Only try to reconnect if we're in interactive mode
                            if self.can_reconnect() {
                                // Try to reconnect on connection close
                                match self.reconnect().await {
                                    Ok(()) => {},
                                    Err(e) => {
                                        if e.to_string().contains("reconnected but needs re-initialization") {
                                            bail!("{}", e);
                                        }
                                        if e.to_string().contains("Maximum reconnection attempts") {
                                            bail!("WebSocket connection closed unexpectedly (Max reconnects reached)");
                                        }
                                        bail!("WebSocket connection closed unexpectedly and reconnect failed: {}", e);
                                    }
                                }
                            } else {
                                // In exec_command mode, just report the close
                                bail!("WebSocket connection closed unexpectedly (exec_command mode)");
                            }
                        }
                    }
                },
                _ = ping_interval.tick() => {
                    if let Err(e) = self.send_ping().await {
                        if e.to_string().contains("reconnected but needs re-initialization") {
                            bail!("{}", e);
                        }
                        if e.to_string().contains("Maximum reconnection attempts") {
                            bail!("Max reconnects reached: {}", e);
                        }
                        // Only try to reconnect if we're in interactive mode
                        if self.can_reconnect() {
                            // If ping fails, try to reconnect
                            match self.reconnect().await {
                                Ok(()) => {},
                                Err(reconnect_err) => {
                                    if reconnect_err.to_string().contains("reconnected but needs re-initialization") {
                                        bail!("{}", reconnect_err);
                                    }
                                    if reconnect_err.to_string().contains("Maximum reconnection attempts") {
                                        bail!("Ping failed (Max reconnects reached)");
                                    }
                                    bail!("Ping failed: {} (Reconnect failed: {})", e, reconnect_err);
                                }
                            }
                        } else {
                            // In exec_command mode, just report the ping failure
                            bail!("Ping failed in exec_command mode: {}", e);
                        }
                    }
                }
            }
        }
    }

    /// Check if the shell is ready for input
    pub fn is_ready(&self) -> bool {
        self.ready
    }
}
