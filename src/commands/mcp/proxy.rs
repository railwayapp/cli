//! Stdio ↔ streamable-HTTP proxy for the remote Railway MCP server.
//!
//! Bridges a harness speaking MCP over stdio (Claude Code, Cursor, …) to
//! `mcp.railway.com`, attaching a fresh `Authorization: Bearer` from the CLI's
//! stored login on every request. This lets users who have already run
//! `railway login` use the remote MCP server without going through the
//! harness's OAuth (DCR + browser consent) flow, and without ever writing a
//! long-lived credential into the harness config.
//!
//! Auth freshness is delegated to [`crate::client::ensure_valid_token`], which
//! serializes refreshes across concurrent CLI processes via the config
//! lockfile — the proxy can safely run alongside the local `railway mcp`
//! server and any other CLI invocations.
//!
//! The remote server currently runs the streamable-HTTP transport statelessly
//! (no `Mcp-Session-Id` issued), but the proxy tracks a session id anyway and
//! transparently re-initializes + retries once if the server ever reports a
//! missing/expired session. Server-initiated messages (the optional GET SSE
//! stream) are not proxied; nothing in the current tool surface relies on
//! them.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::{Value as JsonValue, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use crate::client::ensure_valid_token;
use crate::commands::Environment;
use crate::config::Configs;
use crate::consts;

/// JSON-RPC error code for auth failures surfaced by the proxy itself.
const AUTH_ERROR_CODE: i64 = -32001;

/// Hard ceiling on a single upstream response (one SSE stream, or a non-SSE
/// body). The proxy runs long-lived and attaches a live credential on every
/// request, so a compromised edge (or a dev/http override MITM) streaming a
/// boundary-less or unbounded body must not be able to grow memory without
/// limit. Generous enough for the largest legitimate tool payloads.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

const LOGIN_HINT: &str = "Not logged in to Railway. Run `railway login` in a terminal, then retry \
     — the proxy picks up the new login automatically, no restart needed.";

struct ProxyState {
    http: reqwest::Client,
    url: String,
    configs: Mutex<Configs>,
    session: Mutex<SessionMeta>,
}

#[derive(Default)]
struct SessionMeta {
    id: Option<String>,
    /// The harness's `initialize` request, kept so the proxy can re-establish
    /// an upstream session (expiry, or a degraded logged-out start) without
    /// involving the harness.
    init_request: Option<JsonValue>,
}

type Out = mpsc::UnboundedSender<String>;

pub async fn serve_proxy() -> Result<()> {
    let configs = Configs::new()?;
    let url = resolve_mcp_url(&configs)?;

    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
        .user_agent(consts::get_user_agent())
        .connect_timeout(Duration::from_secs(15))
        // An MCP JSON-RPC POST is never legitimately redirected. Following
        // redirects on a request that carries a Bearer is unnecessary attack
        // surface — reqwest strips the token cross-host, but a same-host 307/308
        // would re-send the body and the redirected response is relayed blind.
        // Refuse redirects outright.
        .redirect(reqwest::redirect::Policy::none())
        // No overall timeout: tool calls (e.g. railway-agent) can legitimately
        // stream for minutes.
        .build()
        .context("Failed to build HTTP client")?;

    let state = Arc::new(ProxyState {
        http,
        url,
        configs: Mutex::new(configs),
        session: Mutex::new(SessionMeta::default()),
    });

    // All stdout writes go through one task so concurrent responses can't
    // interleave within a line.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            let _ = stdout.write_all(line.as_bytes()).await;
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
        }
    });

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    // Process messages inline until the MCP handshake completes so the
    // initialize → initialized ordering is preserved upstream, then handle
    // messages concurrently (harnesses issue parallel tool calls).
    let mut handshake_done = false;

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: JsonValue = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("railway mcp proxy: ignoring unparseable message: {e}");
                continue;
            }
        };

        if method_of(&msg) == Some("initialize") {
            state.session.lock().await.init_request = Some(msg.clone());
        }

        if handshake_done {
            let state = state.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                handle_message(&state, msg, &tx).await;
            });
        } else {
            let completes_handshake = method_of(&msg) == Some("notifications/initialized");
            handle_message(&state, msg, &tx).await;
            if completes_handshake {
                handshake_done = true;
            }
        }
    }

    // stdin closed: the harness is gone. Best-effort end of the remote
    // session, then a bounded wait for in-flight tasks — a stalled upstream
    // stream must not keep an orphaned proxy alive after the harness exits.
    end_session(&state).await;
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
    Ok(())
}

fn method_of(msg: &JsonValue) -> Option<&str> {
    msg.get("method").and_then(JsonValue::as_str)
}

/// Request ids awaiting a response in this message — one for a plain request,
/// several for a JSON-RPC batch (protocol ≤2025-03-26 allows top-level
/// arrays), none for notifications. Error paths must answer every id or the
/// harness waits forever.
fn ids_of(msg: &JsonValue) -> Vec<JsonValue> {
    match msg {
        JsonValue::Array(items) => items
            .iter()
            .filter_map(|m| m.get("id").cloned().filter(|id| !id.is_null()))
            .collect(),
        _ => msg
            .get("id")
            .cloned()
            .filter(|id| !id.is_null())
            .into_iter()
            .collect(),
    }
}

fn resolve_mcp_url(configs: &Configs) -> Result<String> {
    let is_dev = matches!(Configs::get_environment_id(), Environment::Dev);
    if let Ok(raw) = std::env::var("RAILWAY_MCP_URL") {
        if let Some(url) = validate_mcp_override(&raw, is_dev)? {
            return Ok(url);
        }
    }
    Ok(format!("https://mcp.{}", configs.get_host()))
}

/// Validate a `RAILWAY_MCP_URL` override. Returns the normalized URL, `None`
/// when the value is blank (caller falls back to the default), or an error
/// when it would send the Bearer over a non-TLS connection.
///
/// The Bearer is attached to every request to this URL, and the cross-host
/// redirect strip does not protect the *first* hop — so a plaintext target
/// leaks the credential outright. Require https except in the local Dev
/// environment, where a plaintext raildev endpoint is expected.
fn validate_mcp_override(raw: &str, is_dev: bool) -> Result<Option<String>> {
    let url = raw.trim();
    if url.is_empty() {
        return Ok(None);
    }
    if !url.starts_with("https://") && !is_dev {
        anyhow::bail!(
            "RAILWAY_MCP_URL must be an https:// URL (got {url:?}); refusing to send credentials over a non-TLS connection."
        );
    }
    Ok(Some(url.trim_end_matches('/').to_string()))
}

async fn handle_message(state: &ProxyState, msg: JsonValue, out: &Out) {
    let ids = ids_of(&msg);

    let Some(token) = fresh_token(state).await else {
        respond_unauthenticated(&msg, &ids, out);
        return;
    };

    if let Err(e) = forward(state, &msg, &token, out).await {
        if ids.is_empty() {
            eprintln!("railway mcp proxy: {e:#}");
        }
        for id in &ids {
            send_error(out, id, -32603, &format!("Railway MCP proxy error: {e:#}"));
        }
    }
}

/// Get a currently-valid auth token, refreshing the stored OAuth credentials
/// if they have expired. Returns `None` when the user has no usable login.
async fn fresh_token(state: &ProxyState) -> Option<String> {
    let mut configs = state.configs.lock().await;
    // A proxy started before `railway login` has no token in memory, and
    // `ensure_valid_token`'s fast path never re-reads the config in that
    // state. Reload from disk so a login that happened after startup is
    // picked up — this is what makes the LOGIN_HINT's "no restart needed"
    // promise true.
    if configs.get_railway_auth_token().is_none() {
        if let Err(e) = configs.reload() {
            eprintln!("railway mcp proxy: config reload failed: {e:#}");
        }
    }
    if let Err(e) = ensure_valid_token(&mut configs).await {
        // On `invalid_grant` the dead credential has already been cleared, so
        // `get_railway_auth_token()` below returns None and the caller answers
        // with LOGIN_HINT — actionable inside an MCP harness, where this
        // stderr line is invisible. Previously the stale token was handed back
        // and every tool call failed as an opaque "Unauthorized" instead.
        eprintln!("railway mcp proxy: token refresh failed: {e:#}");
    }
    configs.get_railway_auth_token()
}

/// Without a login the proxy still completes the MCP handshake (a crashed
/// server renders as an opaque failure in most harnesses) and answers every
/// request with an actionable error. Once the user logs in, the next request
/// heals automatically via the re-initialize path in [`forward`].
fn respond_unauthenticated(msg: &JsonValue, ids: &[JsonValue], out: &Out) {
    let [id] = ids else {
        // Batch or notification: answer every id (none for a notification).
        for id in ids {
            send_error(out, id, AUTH_ERROR_CODE, LOGIN_HINT);
        }
        return;
    };

    if method_of(msg) == Some("initialize") {
        let protocol = msg
            .pointer("/params/protocolVersion")
            .and_then(JsonValue::as_str)
            .unwrap_or("2025-03-26");
        let result = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": protocol,
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": {
                    "name": "railway",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": LOGIN_HINT,
            }
        });
        let _ = out.send(result.to_string());
    } else {
        send_error(out, id, AUTH_ERROR_CODE, LOGIN_HINT);
    }
}

async fn forward(state: &ProxyState, msg: &JsonValue, token: &str, out: &Out) -> Result<()> {
    let is_initialize = method_of(msg) == Some("initialize");
    let session_id = state.session.lock().await.id.clone();

    let resp = post_message(
        state,
        msg,
        token,
        if is_initialize {
            None
        } else {
            session_id.as_deref()
        },
    )
    .await?;

    // Session lost or never established upstream (server-side expiry, or the
    // proxy started degraded while logged out): re-initialize with the
    // captured initialize request and retry once.
    let status = resp.status();
    let can_reinit = { state.session.lock().await.init_request.is_some() };
    if !is_initialize && (status == 404 || status == 400) && can_reinit {
        let _ = resp.bytes().await;
        reinitialize(state, token).await?;
        let session_id = state.session.lock().await.id.clone();
        let resp = post_message(state, msg, token, session_id.as_deref()).await?;
        return consume_response(state, resp, msg, is_initialize, out).await;
    }

    consume_response(state, resp, msg, is_initialize, out).await
}

async fn post_message(
    state: &ProxyState,
    msg: &JsonValue,
    token: &str,
    session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let mut req = state
        .http
        .post(&state.url)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json, text/event-stream")
        .header("x-source", consts::get_user_agent());
    if let Some(sid) = session_id {
        req = req.header("mcp-session-id", sid);
    }
    req.json(msg)
        .send()
        .await
        .context("failed to reach the remote MCP server")
}

/// Re-run the MCP handshake upstream using the harness's captured `initialize`
/// request, discarding the result (the harness already completed its own
/// handshake). Serialized behind the session lock so concurrent failures
/// don't stampede.
async fn reinitialize(state: &ProxyState, token: &str) -> Result<()> {
    let mut session = state.session.lock().await;
    let init = session
        .init_request
        .clone()
        .context("no initialize request captured yet")?;

    let resp = post_message(state, &init, token, None).await?;
    anyhow::ensure!(
        resp.status().is_success(),
        "re-initialize failed with HTTP {}",
        resp.status()
    );
    session.id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let _ = resp.bytes().await;
    let session_id = session.id.clone();
    drop(session);

    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let resp = post_message(state, &initialized, token, session_id.as_deref()).await?;
    let _ = resp.bytes().await;
    Ok(())
}

async fn consume_response(
    state: &ProxyState,
    resp: reqwest::Response,
    msg: &JsonValue,
    is_initialize: bool,
    out: &Out,
) -> Result<()> {
    let status = resp.status();

    if is_initialize {
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            state.session.lock().await.id = Some(sid.to_string());
        }
    }

    if status == 401 || status == 403 {
        let _ = resp.bytes().await;
        for id in &ids_of(msg) {
            send_error(
                out,
                id,
                AUTH_ERROR_CODE,
                "Railway rejected the CLI's credentials. Run `railway login` and try again.",
            );
        }
        return Ok(());
    }

    // Accepted notification/response with no body.
    if status == 202 || status == 204 {
        return Ok(());
    }

    if !status.is_success() {
        let body = read_body_capped(resp).await.unwrap_or_default();
        anyhow::bail!(
            "remote MCP server returned HTTP {status}: {}",
            truncate(&body, 300)
        );
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if content_type.starts_with("text/event-stream") {
        stream_sse(resp, out).await
    } else {
        let body = read_body_capped(resp).await?;
        emit_json_line(body.trim(), out);
        Ok(())
    }
}

/// Read a full (non-streaming) response body, refusing to buffer more than
/// [`MAX_RESPONSE_BYTES`]. `reqwest`'s `text()`/`bytes()` have no size cap, so
/// a compromised or MITM'd upstream could otherwise stream an unbounded body
/// into a long-lived proxy and exhaust memory.
async fn read_body_capped(resp: reqwest::Response) -> Result<String> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading response from remote MCP server")?;
        if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "remote MCP server response exceeded {MAX_RESPONSE_BYTES} bytes; aborting."
            );
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Relay every SSE `data:` payload to stdout as its own JSON-RPC line. The
/// server closes the per-request stream after the final response message.
async fn stream_sse(resp: reqwest::Response, out: &Out) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE stream from remote MCP server")?;
        buf.extend_from_slice(&chunk);
        while let Some((event_len, boundary_end)) = find_event_boundary(&buf) {
            let event: Vec<u8> = buf.drain(..boundary_end).collect();
            emit_sse_event(&event[..event_len], out);
        }
        // A boundary-less stream (or one giant event) would otherwise grow buf
        // without limit. Cap it: past the ceiling, no legitimate single SSE
        // event is pending — abort rather than let a bad upstream OOM us.
        if buf.len() > MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "remote MCP server SSE event exceeded {MAX_RESPONSE_BYTES} bytes; aborting."
            );
        }
    }
    if !buf.is_empty() {
        emit_sse_event(&buf, out);
    }
    Ok(())
}

/// Find the end of the next SSE event: a blank line, i.e. `\n\n` or
/// `\r\n\r\n`. Returns (event bytes length, total length including boundary).
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    for i in 0..buf.len() {
        if buf[i] != b'\n' {
            continue;
        }
        if buf.get(i + 1) == Some(&b'\n') {
            return Some((i, i + 2));
        }
        if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
            return Some((i, i + 3));
        }
    }
    None
}

fn emit_sse_event(raw: &[u8], out: &Out) {
    let text = String::from_utf8_lossy(raw);
    let data_lines: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
        .collect();
    if data_lines.is_empty() {
        return;
    }
    emit_json_line(&data_lines.join("\n"), out);
}

/// Write one JSON-RPC message as a single stdout line. Payloads are compacted
/// through serde so an upstream message containing raw newlines can't corrupt
/// the newline-delimited stdio framing.
fn emit_json_line(payload: &str, out: &Out) {
    if payload.is_empty() {
        return;
    }
    let line = serde_json::from_str::<JsonValue>(payload)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| payload.replace(['\n', '\r'], " "));
    let _ = out.send(line);
}

fn send_error(out: &Out, id: &JsonValue, code: i64, message: &str) {
    let err = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    let _ = out.send(err.to_string());
}

/// Best-effort session teardown when the harness disconnects.
async fn end_session(state: &ProxyState) {
    let session_id = state.session.lock().await.id.clone();
    let Some(session_id) = session_id else { return };
    let token = { state.configs.lock().await.get_railway_auth_token() };
    let Some(token) = token else { return };
    let _ = state
        .http
        .delete(&state.url)
        .header("authorization", format!("Bearer {token}"))
        .header("mcp-session-id", session_id)
        .timeout(Duration::from_secs(5))
        .send()
        .await;
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(line) = rx.try_recv() {
            out.push(line);
        }
        out
    }

    #[test]
    fn mcp_override_rejects_plaintext_outside_dev() {
        // http:// would send the Bearer in the clear — refused in prod/staging.
        assert!(validate_mcp_override("http://evil.example/mcp", false).is_err());
        // Blank falls back to the default (Ok(None), not an error).
        assert_eq!(validate_mcp_override("  ", false).unwrap(), None);
        // https is accepted and trailing slashes normalized.
        assert_eq!(
            validate_mcp_override("https://mcp.railway.com/", false).unwrap(),
            Some("https://mcp.railway.com".to_string()),
        );
        // Dev allows plaintext for a local raildev endpoint.
        assert_eq!(
            validate_mcp_override("http://localhost:8080", true).unwrap(),
            Some("http://localhost:8080".to_string()),
        );
    }

    #[test]
    fn sse_event_boundary_handles_lf_and_crlf() {
        assert_eq!(find_event_boundary(b"data: {}\n\nrest"), Some((8, 10)));
        assert_eq!(find_event_boundary(b"data: {}\r\n\r\nrest"), Some((9, 12)));
        assert_eq!(find_event_boundary(b"data: {}"), None);
    }

    #[test]
    fn sse_event_extracts_data_payload() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_sse_event(b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1}", &tx);
        assert_eq!(collect(&mut rx), vec![r#"{"id":1,"jsonrpc":"2.0"}"#]);
    }

    #[test]
    fn sse_event_joins_multiline_data() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_sse_event(b"data: {\"a\":\ndata: 1}", &tx);
        assert_eq!(collect(&mut rx), vec![r#"{"a":1}"#]);
    }

    #[test]
    fn sse_event_without_data_is_dropped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_sse_event(b"event: ping\nid: 4", &tx);
        assert!(collect(&mut rx).is_empty());
    }

    #[test]
    fn json_lines_are_compacted_to_one_line() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_json_line("{\n  \"jsonrpc\": \"2.0\",\n  \"id\": 7\n}", &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains('\n'));
    }

    #[test]
    fn unauthenticated_initialize_fabricates_result() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" },
        });
        respond_unauthenticated(&msg, &ids_of(&msg), &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 1);
        let parsed: JsonValue = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(
            parsed.pointer("/result/protocolVersion").unwrap(),
            "2025-06-18"
        );
        assert!(
            parsed
                .pointer("/result/instructions")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("railway login")
        );
    }

    #[test]
    fn unauthenticated_request_gets_actionable_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" });
        respond_unauthenticated(&msg, &ids_of(&msg), &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 1);
        let parsed: JsonValue = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parsed.pointer("/error/code").unwrap(), AUTH_ERROR_CODE);
        assert!(
            parsed
                .pointer("/error/message")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("railway login")
        );
    }

    #[test]
    fn unauthenticated_notification_is_dropped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        respond_unauthenticated(&msg, &ids_of(&msg), &tx);
        assert!(collect(&mut rx).is_empty());
    }

    #[test]
    fn unauthenticated_batch_answers_every_id() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "tools/list" },
            { "jsonrpc": "2.0", "method": "notifications/progress" },
            { "jsonrpc": "2.0", "id": "two", "method": "tools/call" },
        ]);
        let ids = ids_of(&msg);
        assert_eq!(ids, vec![json!(1), json!("two")]);
        respond_unauthenticated(&msg, &ids, &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let parsed: JsonValue = serde_json::from_str(line).unwrap();
            assert_eq!(parsed.pointer("/error/code").unwrap(), AUTH_ERROR_CODE);
        }
    }
}
