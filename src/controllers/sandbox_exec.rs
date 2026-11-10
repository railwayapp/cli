//! Streaming sandbox exec over the tcp-proxy `/ws/exec` WebSocket bridge.
//!
//! Replaces the old blocking `sandboxExec` GraphQL mutation, which buffered
//! all output and died at the HTTP proxy's ~1 minute limit. The bridge
//! streams stdout/stderr live (tagged binary frames), accepts stdin, delivers
//! real signals, and — when the sandbox VM supports it — assigns a *durable
//! session* that survives disconnects and can be reattached to by name.
//!
//! Wire protocol (source of truth: mono `tcp-proxy/handlers/ssh_listener/ws_bridge_exec.go`):
//! - auth: a `shell`-scoped JWT travels as the last `Sec-WebSocket-Protocol`
//!   value alongside `railway-shell`
//! - client → server text frames: `init_exec` (first), `stdin_close`, `signal`
//! - server → client text frames: `durable_session`, `exit`
//! - binary frames carry a stream tag in the first byte; the rest is raw bytes

use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::{SinkExt, StreamExt};
use reqwest_websocket::{CloseCode, Message, RequestBuilderExt, WebSocket};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::Configs;

/// Subprotocol the tcp-proxy bridge expects alongside the JWT.
const SHELL_SUBPROTOCOL: &str = "railway-shell";

/// Binary frame stream tags (first byte).
pub const STREAM_STDOUT: u8 = 0x01;
pub const STREAM_STDIN: u8 = 0x02;
pub const STREAM_STDERR: u8 = 0x03;

/// Exit code for a client-side `--timeout` expiry, per GNU timeout convention.
pub const TIMEOUT_EXIT_CODE: i32 = 124;

/// The bridge requires a non-empty command even on reattach; the VM ignores it
/// when the durable session name resolves to a live session.
const REATTACH_PLACEHOLDER_COMMAND: &str = ":";

/// Options for a single exec run.
pub struct ExecOptions {
    /// Command to run; may be `None` only when reattaching to a session.
    pub command: Option<String>,
    /// Reattach to this durable session instead of starting fresh.
    pub session: Option<String>,
    /// On reattach, resume from the server's last-read cursor instead of
    /// replaying the full retained log.
    pub resume_from_last_read: bool,
    /// Client-side deadline; on expiry the command is TERMed.
    pub timeout: Option<Duration>,
    /// Return as soon as the server assigns a durable session, leaving the
    /// command running.
    pub detach: bool,
    /// When stdin is a TTY we never read it — EOF is sent immediately so
    /// commands that read stdin can finish. Piped stdin is forwarded.
    pub stdin_is_tty: bool,
}

/// How an exec run ended. `session_name` is the durable session (server-assigned
/// or the reattach target) when one is known — the reattach handle.
pub enum ExecOutcome {
    /// The command exited with this code. `fresh_session_suspected` flags the
    /// expired-reattach footgun: a `--session` run that instantly exits 0 with
    /// no output almost certainly hit an expired id (the bridge silently
    /// starts a fresh no-op session for unknown ids).
    Exited {
        code: i32,
        fresh_session_suspected: bool,
    },
    /// The client-side `--timeout` expired; the command was sent TERM.
    TimedOut { session_name: Option<String> },
    /// `--detach`: the durable session was assigned and the socket released
    /// with the command still running.
    Detached { session_name: String },
    /// The socket closed (or the user force-quit) before an exit frame.
    Disconnected { session_name: Option<String> },
}

/// Derive the `/ws/exec` endpoint for the current environment, honoring the
/// `RAILWAY_TCP_PROXY_WS_ENDPOINT` override (full endpoint URL, SDK parity).
pub fn ws_exec_endpoint() -> String {
    let override_endpoint = std::env::var("RAILWAY_TCP_PROXY_WS_ENDPOINT").ok();
    ws_exec_endpoint_from(Configs::get_ssh_relay().0, override_endpoint.as_deref())
}

fn ws_exec_endpoint_from(relay_host: &str, override_endpoint: Option<&str>) -> String {
    if let Some(endpoint) = override_endpoint {
        let trimmed = endpoint.trim();
        if !trimmed.is_empty() {
            return trimmed.trim_end_matches('/').to_string();
        }
    }
    // The WS bridge listens on 2226 in every environment (unlike the SSH
    // relay port, which varies), so only the host comes from the relay config.
    format!("wss://{relay_host}:2226/ws/exec")
}

/// Open the bridge WebSocket. A fresh client is used on purpose: the
/// authorized GraphQL client carries a global request timeout that would kill
/// a long-lived upgraded connection, and auth here travels in the subprotocol,
/// not headers (same pattern as `subscription.rs`).
pub async fn connect(jwt: &str) -> Result<WebSocket> {
    let endpoint = ws_exec_endpoint();
    let response = reqwest::Client::default()
        .get(&endpoint)
        .timeout(Duration::from_secs(15))
        .upgrade()
        .protocols([SHELL_SUBPROTOCOL.to_string(), jwt.to_string()])
        .send()
        .await
        .with_context(|| format!("Failed to connect to {endpoint}"))?;
    response.error_for_status_ref()?;
    Ok(response.into_websocket().await?)
}

fn encode_stdin_frame(bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + bytes.len());
    frame.push(STREAM_STDIN);
    frame.extend_from_slice(bytes);
    frame
}

/// Split a tagged binary frame into `(stream_tag, payload)`. Frames without a
/// payload carry nothing actionable and decode to `None`.
fn decode_binary_frame(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.len() <= 1 {
        return None;
    }
    Some((data[0], &data[1..]))
}

fn init_exec_payload(opts: &ExecOptions) -> serde_json::Value {
    let mut data = json!({
        "command": opts
            .command
            .as_deref()
            .unwrap_or(REATTACH_PLACEHOLDER_COMMAND),
    });
    if let Some(session) = &opts.session {
        data["durable_session_name"] = json!(session);
        if opts.resume_from_last_read {
            data["resume_from_last_read"] = json!(true);
        }
    }
    json!({ "type": "init_exec", "data": data })
}

fn stdin_close_payload() -> String {
    json!({ "type": "stdin_close" }).to_string()
}

fn signal_payload(signal: &str) -> String {
    json!({ "type": "signal", "data": { "signal": signal } }).to_string()
}

/// A parsed server text frame; unknown types and malformed JSON are ignored,
/// matching the bridge's lenient contract.
#[derive(serde::Deserialize)]
struct WsFrame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    data: serde_json::Value,
}

/// Send `init_exec` and pump the session to completion: server frames fan out
/// to stdout/stderr, piped stdin fans in, Ctrl+C maps to a real INT (twice
/// force-quits), and the optional deadline TERMs the command.
pub async fn run(ws: WebSocket, opts: ExecOptions) -> Result<ExecOutcome> {
    let (mut tx, mut rx) = ws.split();

    tx.send(Message::Text(init_exec_payload(&opts).to_string()))
        .await
        .context("Failed to start command")?;

    let is_reattach = opts.session.is_some();
    let mut session_name = opts.session.clone();
    let mut wrote_output = false;
    let mut interrupted = false;

    // Piped stdin is forwarded; TTY stdin is never read — EOF it up front.
    let mut stdin = if opts.stdin_is_tty {
        tx.send(Message::Text(stdin_close_payload())).await?;
        None
    } else {
        Some(tokio::io::stdin())
    };
    let mut stdin_buf = [0u8; 8192];

    let deadline = opts.timeout.map(|t| tokio::time::Instant::now() + t);

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();

    loop {
        tokio::select! {
            message = rx.next() => match message {
                Some(Ok(Message::Binary(data))) => {
                    if let Some((tag, payload)) = decode_binary_frame(&data) {
                        wrote_output = true;
                        match tag {
                            STREAM_STDOUT => {
                                stdout.write_all(payload).await?;
                                stdout.flush().await?;
                            }
                            STREAM_STDERR => {
                                stderr.write_all(payload).await?;
                                stderr.flush().await?;
                            }
                            _ => {}
                        }
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    let Ok(frame) = serde_json::from_str::<WsFrame>(&text) else {
                        continue;
                    };
                    match frame.kind.as_str() {
                        "durable_session" => {
                            if let Some(name) = frame.data["durable_session_name"].as_str() {
                                session_name = Some(name.to_string());
                                if opts.detach {
                                    // Dropping the socket releases the stream
                                    // without ending the command.
                                    return Ok(ExecOutcome::Detached {
                                        session_name: name.to_string(),
                                    });
                                }
                            }
                        }
                        "exit" => {
                            let code = frame.data["exit_code"].as_i64().unwrap_or(0) as i32;
                            return Ok(ExecOutcome::Exited {
                                code,
                                fresh_session_suspected: is_reattach
                                    && code == 0
                                    && !wrote_output,
                            });
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Close { code, reason })) => {
                    if !wrote_output && !interrupted {
                        bail!(
                            "connection closed before the command produced output \
                             (code {code:?}{})",
                            if reason.is_empty() {
                                String::new()
                            } else {
                                format!(": {reason}")
                            }
                        );
                    }
                    return Ok(ExecOutcome::Disconnected { session_name });
                }
                Some(Ok(_)) => {} // ping/pong handled by the transport
                Some(Err(e)) => return Err(e).context("exec stream failed"),
                None => return Ok(ExecOutcome::Disconnected { session_name }),
            },

            // Forward piped stdin; EOF half-closes so readers can finish.
            read = async { stdin.as_mut().expect("guarded by arm condition").read(&mut stdin_buf).await },
                if stdin.is_some() =>
            {
                match read {
                    Ok(0) | Err(_) => {
                        tx.send(Message::Text(stdin_close_payload())).await?;
                        stdin = None;
                    }
                    Ok(n) => {
                        tx.send(Message::Binary(encode_stdin_frame(&stdin_buf[..n]).into()))
                            .await?;
                    }
                }
            }

            // First Ctrl+C interrupts the remote command; second force-quits
            // the stream (with durable sessions the command keeps running).
            _ = tokio::signal::ctrl_c() => {
                if interrupted {
                    let _ = tx
                        .send(Message::Close {
                            code: CloseCode::Normal,
                            reason: "client interrupt".into(),
                        })
                        .await;
                    return Ok(ExecOutcome::Disconnected { session_name });
                }
                interrupted = true;
                let _ = tx.send(Message::Text(signal_payload("INT"))).await;
            }

            // Client-side deadline: TERM the command, then let go.
            _ = async { tokio::time::sleep_until(deadline.expect("guarded by arm condition")).await },
                if deadline.is_some() =>
            {
                let _ = tx.send(Message::Text(signal_payload("TERM"))).await;
                let _ = tx
                    .send(Message::Close {
                        code: CloseCode::Normal,
                        reason: "timeout".into(),
                    })
                    .await;
                return Ok(ExecOutcome::TimedOut { session_name });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(command: Option<&str>, session: Option<&str>, resume: bool) -> ExecOptions {
        ExecOptions {
            command: command.map(String::from),
            session: session.map(String::from),
            resume_from_last_read: resume,
            timeout: None,
            detach: false,
            stdin_is_tty: true,
        }
    }

    #[test]
    fn stdin_frame_prepends_tag() {
        assert_eq!(encode_stdin_frame(b"hi"), vec![STREAM_STDIN, b'h', b'i']);
        assert_eq!(encode_stdin_frame(b""), vec![STREAM_STDIN]);
    }

    #[test]
    fn binary_frame_decodes_tag_and_payload() {
        assert_eq!(
            decode_binary_frame(&[STREAM_STDOUT, b'a', b'b']),
            Some((STREAM_STDOUT, b"ab".as_slice()))
        );
        assert_eq!(
            decode_binary_frame(&[STREAM_STDERR, b'x']),
            Some((STREAM_STDERR, b"x".as_slice()))
        );
        // Tag-only and empty frames carry no payload.
        assert_eq!(decode_binary_frame(&[STREAM_STDOUT]), None);
        assert_eq!(decode_binary_frame(&[]), None);
    }

    #[test]
    fn init_payload_fresh_command() {
        let payload = init_exec_payload(&opts(Some("echo hi"), None, false));
        assert_eq!(
            payload,
            serde_json::json!({
                "type": "init_exec",
                "data": { "command": "echo hi" }
            })
        );
    }

    #[test]
    fn init_payload_reattach_uses_placeholder() {
        let payload = init_exec_payload(&opts(None, Some("sess-1"), false));
        assert_eq!(payload["data"]["command"], ":");
        assert_eq!(payload["data"]["durable_session_name"], "sess-1");
        assert!(payload["data"].get("resume_from_last_read").is_none());
    }

    #[test]
    fn init_payload_resume_from_last_read() {
        let payload = init_exec_payload(&opts(None, Some("sess-1"), true));
        assert_eq!(payload["data"]["resume_from_last_read"], true);
    }

    #[test]
    fn endpoint_derives_from_relay_host() {
        assert_eq!(
            ws_exec_endpoint_from("ssh.railway.com", None),
            "wss://ssh.railway.com:2226/ws/exec"
        );
        assert_eq!(
            ws_exec_endpoint_from("ssh.railway-develop.com", None),
            "wss://ssh.railway-develop.com:2226/ws/exec"
        );
    }

    #[test]
    fn endpoint_override_wins_and_is_trimmed() {
        assert_eq!(
            ws_exec_endpoint_from("ssh.railway.com", Some("wss://localhost:2226/ws/exec/")),
            "wss://localhost:2226/ws/exec"
        );
        // Blank overrides fall through to derivation.
        assert_eq!(
            ws_exec_endpoint_from("ssh.railway.com", Some("  ")),
            "wss://ssh.railway.com:2226/ws/exec"
        );
    }
}
